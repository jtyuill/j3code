import SwiftUI
import UIKit

/// Parsed markdown by source text, so a re-rendered row does not parse again.
enum MarkdownCache {
    private final class Entry {
        let markdown: Markdown
        init(_ markdown: Markdown) { self.markdown = markdown }
    }

    nonisolated(unsafe) private static let cache: NSCache<NSString, Entry> = {
        let cache = NSCache<NSString, Entry>()
        cache.countLimit = 500
        return cache
    }()

    static func markdown(for text: String) -> Markdown {
        let key = text as NSString
        if let entry = cache.object(forKey: key) { return entry.markdown }
        let markdown = parseMarkdown(markdown: text)
        cache.setObject(Entry(markdown), forKey: key)
        return markdown
    }
}

/// A typeset formula as an image, with the offset that puts its baseline on
/// the text's.
struct FormulaImage {
    let image: UIImage
    let baselineOffset: CGFloat
}

/// Rasterized formulas by TeX, style, size and color.
enum FormulaImages {
    private final class Entry {
        let image: FormulaImage?
        init(_ image: FormulaImage?) { self.image = image }
    }

    nonisolated(unsafe) private static let cache: NSCache<NSString, Entry> = {
        let cache = NSCache<NSString, Entry>()
        cache.countLimit = 400
        return cache
    }()

    static func image(for formula: Formula, ink: UIColor, pointSize: CGFloat, scale: CGFloat) -> FormulaImage? {
        var red: CGFloat = 0, green: CGFloat = 0, blue: CGFloat = 0, alpha: CGFloat = 0
        ink.getRed(&red, green: &green, blue: &blue, alpha: &alpha)
        let key = "\(formula.display)|\(pointSize)|\(scale)|\(red),\(green),\(blue),\(alpha)|\(formula.tex)" as NSString
        if let entry = cache.object(forKey: key) { return entry.image }
        let image = render(formula, red: red, green: green, blue: blue, alpha: alpha, pointSize: pointSize, scale: scale)
        cache.setObject(Entry(image), forKey: key)
        return image
    }

    private static func render(
        _ formula: Formula, red: CGFloat, green: CGFloat, blue: CGFloat, alpha: CGFloat,
        pointSize: CGFloat, scale: CGFloat
    ) -> FormulaImage? {
        guard let bitmap = rasterizeFormula(
            formula: formula, red: Float(red), green: Float(green), blue: Float(blue), alpha: Float(alpha),
            fontSize: Float(pointSize * scale)
        ) else { return nil }
        let width = Int(bitmap.width), height = Int(bitmap.height)
        guard bitmap.rgba.count == width * height * 4,
              let provider = CGDataProvider(data: bitmap.rgba as CFData),
              let space = CGColorSpace(name: CGColorSpace.sRGB),
              let cgImage = CGImage(
                  width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
                  bytesPerRow: width * 4, space: space,
                  bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                  provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent
              )
        else { return nil }
        let image = UIImage(cgImage: cgImage, scale: scale, orientation: .up)
        return FormulaImage(image: image, baselineOffset: -(CGFloat(height) - CGFloat(bitmap.ascent)) / scale)
    }
}

/// Builds `Text` from markdown inlines: styled runs, links, inline formulas.
struct InlineRenderer {
    let formulas: [Formula]
    /// The resolved text color formulas are drawn in.
    let ink: UIColor
    let scale: CGFloat
    let sizeCategory: UIContentSizeCategory

    func pointSize(_ style: Font.TextStyle) -> CGFloat {
        let traits = UITraitCollection(preferredContentSizeCategory: sizeCategory)
        return UIFont.preferredFont(forTextStyle: style.uiStyle, compatibleWith: traits).pointSize
    }

    func formula(_ index: UInt32, style: Font.TextStyle) -> (Formula, FormulaImage?)? {
        guard formulas.indices.contains(Int(index)) else { return nil }
        let formula = formulas[Int(index)]
        return (formula, FormulaImages.image(for: formula, ink: ink, pointSize: pointSize(style), scale: scale))
    }

    func text(_ inlines: [MdInline], style: Font.TextStyle, bold: Bool = false) -> Text {
        var result: Text?
        var pending = AttributedString()

        func append(_ text: Text) {
            result = result.map { Text("\($0)\(text)") } ?? text
        }
        func flush() {
            guard !pending.characters.isEmpty else { return }
            append(Text(pending))
            pending = AttributedString()
        }

        for inline in inlines {
            switch inline {
            case let .text(text, isBold, italic, strike, code, link):
                var run = AttributedString(text)
                var font = Font.system(style, design: code ? .monospaced : .default)
                if bold || isBold { font = font.bold() }
                if italic { font = font.italic() }
                run.font = font
                if strike { run.strikethroughStyle = .single }
                if code { run.backgroundColor = Color(.tertiarySystemFill) }
                if let link, let url = URL(string: link) { run.link = url }
                pending += run
            case let .math(index):
                guard let (formula, image) = self.formula(index, style: style) else { continue }
                if let image {
                    flush()
                    append(
                        Text(Image(uiImage: image.image))
                            .baselineOffset(image.baselineOffset)
                            .accessibilityLabel(formula.tex)
                    )
                } else {
                    var run = AttributedString(formula.tex)
                    run.font = .system(style, design: .monospaced)
                    pending += run
                }
            case let .image(url, alt):
                var run = AttributedString(alt.isEmpty ? url : alt)
                run.font = .system(style)
                if let link = URL(string: url) { run.link = link }
                pending += run
            case .break:
                pending += AttributedString("\n")
            }
        }
        flush()
        return result ?? Text(verbatim: "")
    }
}

extension Font.TextStyle {
    var uiStyle: UIFont.TextStyle {
        switch self {
        case .largeTitle: .largeTitle
        case .title: .title1
        case .title2: .title2
        case .title3: .title3
        case .headline: .headline
        case .subheadline: .subheadline
        case .callout: .callout
        case .footnote: .footnote
        case .caption: .caption1
        case .caption2: .caption2
        default: .body
        }
    }
}

/// Assistant markdown (`parseMarkdown`) as native views.
struct MarkdownView: View {
    let text: String
    let host: Host
    /// Drawn in the secondary color (thinking).
    var secondary = false
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.displayScale) private var displayScale
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    var body: some View {
        let markdown = MarkdownCache.markdown(for: text)
        let ink = (secondary ? UIColor.secondaryLabel : UIColor.label)
            .resolvedColor(with: UITraitCollection(userInterfaceStyle: colorScheme == .dark ? .dark : .light))
        let renderer = InlineRenderer(
            formulas: markdown.formulas, ink: ink, scale: displayScale,
            sizeCategory: UIContentSizeCategory(dynamicTypeSize)
        )
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(markdown.blocks.enumerated()), id: \.offset) { _, block in
                MarkdownBlockView(block: block, renderer: renderer, host: host)
            }
        }
        .foregroundStyle(secondary ? .secondary : .primary)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct MarkdownBlockView: View {
    let block: MdBlock
    let renderer: InlineRenderer
    let host: Host
    @Environment(\.media) private var media

    private static let indentStep: CGFloat = 22

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if let marker = block.marker {
                markerView(marker)
                    .frame(minWidth: 16, alignment: .trailing)
            }
            content
        }
        .padding(.leading, listInset)
        .padding(.leading, CGFloat(block.quote) * 12)
        .overlay(alignment: .leading) {
            if block.quote > 0 {
                HStack(spacing: 9) {
                    ForEach(0..<Int(block.quote), id: \.self) { _ in
                        RoundedRectangle(cornerRadius: 1.5)
                            .fill(Color(.tertiaryLabel))
                            .frame(width: 3)
                    }
                }
            }
        }
    }

    /// Lists nest by one step; a block under an item's marker lines up with
    /// the item's text.
    private var listInset: CGFloat {
        guard block.indent > 0 else { return 0 }
        return CGFloat(block.indent - 1) * Self.indentStep + (block.marker == nil ? Self.indentStep : 0)
    }

    @ViewBuilder
    private func markerView(_ marker: ListMarker) -> some View {
        switch marker {
        case .bullet:
            Text(verbatim: "•")
        case let .number(number):
            Text(verbatim: "\(number).").monospacedDigit()
        case let .task(checked):
            Image(systemName: checked ? "checkmark.square.fill" : "square")
                .foregroundStyle(checked ? Color.accentColor : Color.secondary)
                .accessibilityLabel(checked ? "Done" : "Not done")
        }
    }

    @ViewBuilder
    private var content: some View {
        switch block.kind {
        case let .paragraph(inlines):
            renderer.text(inlines, style: .body)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        case let .heading(level, inlines):
            renderer.text(inlines, style: Self.headingStyle(level), bold: true)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 4)
                .accessibilityAddTraits(.isHeader)
        case let .code(language, code):
            CodeBlockView(language: language, code: code)
        case let .math(index):
            if let (formula, image) = renderer.formula(index, style: .body) {
                DisplayFormula(formula: formula, image: image)
            }
        case let .image(url, alt):
            let link = host.resolve(url)
            Button {
                if let link { media?.showImage(link, name: alt.isEmpty ? link.lastPathComponent : alt) }
            } label: {
                RemoteImage(url: link)
                    .frame(maxWidth: .infinity, maxHeight: 320, alignment: .leading)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            }
            .buttonStyle(.plain)
            .accessibilityLabel(alt.isEmpty ? "Image" : alt)
        case let .table(alignments, header, rows):
            MarkdownTable(alignments: alignments, header: header, rows: rows, renderer: renderer)
        case .rule:
            Divider().padding(.vertical, 4)
        }
    }

    private static func headingStyle(_ level: UInt8) -> Font.TextStyle {
        switch level {
        case 1: .title2
        case 2: .title3
        case 3: .headline
        default: .subheadline
        }
    }
}

private struct DisplayFormula: View {
    let formula: Formula
    let image: FormulaImage?

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Group {
                if let image {
                    Image(uiImage: image.image)
                } else {
                    Text(formula.tex).font(.body.monospaced())
                }
            }
            .padding(.vertical, 4)
        }
        .defaultScrollAnchor(.center, for: .alignment)
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        .accessibilityElement()
        .accessibilityLabel(formula.tex)
    }
}

/// A fenced code block: language, copy button, horizontally scrolling text.
struct CodeBlockView: View {
    let language: String?
    let code: String
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(language?.isEmpty == false ? language! : "code")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    UIPasteboard.general.string = code
                    copied = true
                } label: {
                    Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                        .font(.caption)
                }
                .buttonStyle(.borderless)
            }
            .padding(.horizontal, 10)
            .padding(.top, 6)
            ScrollView(.horizontal) {
                Text(verbatim: code.hasSuffix("\n") ? String(code.dropLast()) : code)
                    .font(.callout.monospaced())
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(10)
            }
            .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        }
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        .task(id: copied) {
            guard copied else { return }
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
    }
}

private struct MarkdownTable: View {
    let alignments: [MdAlignment]
    let header: [MdCell]
    let rows: [MdRow]
    let renderer: InlineRenderer

    var body: some View {
        ScrollView(.horizontal) {
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
                GridRow {
                    ForEach(Array(header.enumerated()), id: \.offset) { column, cell in
                        renderer.text(cell.inlines, style: .subheadline, bold: true)
                            .gridColumnAlignment(alignment(column))
                    }
                }
                Divider()
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(Array(row.cells.enumerated()), id: \.offset) { _, cell in
                            renderer.text(cell.inlines, style: .subheadline)
                        }
                    }
                }
            }
            .textSelection(.enabled)
            .padding(.vertical, 4)
        }
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
    }

    private func alignment(_ column: Int) -> HorizontalAlignment {
        guard alignments.indices.contains(column) else { return .leading }
        switch alignments[column] {
        case .none, .left: return .leading
        case .center: return .center
        case .right: return .trailing
        }
    }
}
