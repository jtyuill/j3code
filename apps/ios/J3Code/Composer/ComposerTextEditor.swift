import SwiftUI
import UIKit

/// What the text view asks of its coordinator for the completion menu and
/// submission.
@MainActor
protocol ComposerKeyHandling: AnyObject {
    func moveCompletion(_ delta: Int)
    func acceptCompletion()
    func closeCompletion()
    func handleReturn(shift: Bool, command: Bool)
}

/// The composer's text view: body-font text, one attachment character per
/// chip, the completion keys and paste handling.
final class ComposerTextView: UITextView, ComposerEditing {
    static let maxLines = 8

    weak var keyHandler: (any ComposerKeyHandling)?
    /// Pasted images, handed to the composer instead of being inserted.
    var onPasteImages: (([Data]) -> Void)?

    private var completionOpen = false
    private lazy var completionCommands: [UIKeyCommand] = [
        Self.command(UIKeyCommand.inputUpArrow, [], #selector(handleUp)),
        Self.command(UIKeyCommand.inputDownArrow, [], #selector(handleDown)),
        Self.command("\t", [], #selector(handleTab)),
        Self.command(UIKeyCommand.inputEscape, [], #selector(handleEscape)),
    ]

    override init(frame: CGRect, textContainer: NSTextContainer?) {
        super.init(frame: frame, textContainer: textContainer)
        font = UIFont.preferredFont(forTextStyle: .body)
        adjustsFontForContentSizeCategory = true
        textColor = .label
        backgroundColor = .clear
        isScrollEnabled = true
        textContainerInset = .zero
        textContainer?.lineFragmentPadding = 0
        accessibilityIdentifier = "composer"
        typingAttributes = textAttributes
        registerForTraitChanges([
            UITraitUserInterfaceStyle.self, UITraitDisplayScale.self, UITraitPreferredContentSizeCategory.self,
        ]) { (view: ComposerTextView, _) in
            view.restyle()
        }
    }

    required init?(coder: NSCoder) { nil }

    // MARK: Draft

    var draftText: String { text ?? "" }
    var draftParts: [DraftPart] { Draft.parts(from: attributedText) }
    var cursorOffset: Int { selectedRange.location }

    /// Replaces the text with `parts`; the caret goes to the end when
    /// `cursorAtEnd`, else stays where it can.
    func setParts(_ parts: [DraftPart], cursorAtEnd: Bool) {
        let previous = selectedRange
        let content = Draft.attributedString(parts, attributes: textAttributes) { self.chipAttachment($0) }
        attributedText = content
        typingAttributes = textAttributes
        let length = (text as NSString).length
        selectedRange = NSRange(location: cursorAtEnd ? length : min(previous.location, length), length: 0)
        invalidateIntrinsicContentSize()
        setNeedsLayout()
    }

    /// Replaces `edit.range` with its parts and puts the caret after them.
    func apply(_ edit: CompletionEdit) {
        let storage = textStorage
        guard edit.range.location >= 0, NSMaxRange(edit.range) <= storage.length else { return }
        let replacement = Draft.attributedString(edit.parts, attributes: textAttributes) {
            self.chipAttachment($0)
        }
        storage.beginEditing()
        storage.replaceCharacters(in: edit.range, with: replacement)
        storage.endEditing()
        selectedRange = NSRange(location: edit.range.location + replacement.length, length: 0)
        typingAttributes = textAttributes
        invalidateIntrinsicContentSize()
        setNeedsLayout()
    }

    func focus() { becomeFirstResponder() }

    /// The height of one to `maxLines` lines at `width`.
    func measuredHeight(forWidth width: CGFloat) -> CGFloat {
        let lineHeight = ceil(font?.lineHeight ?? 0)
        guard width > 0 else { return lineHeight }
        let fitting = sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
        return min(max(ceil(fitting), lineHeight), lineHeight * CGFloat(Self.maxLines))
    }

    /// Installs the keys that only apply while the completion menu is open.
    /// Called from a SwiftUI update: reloading the input views resizes the
    /// keyboard's tracking area, which renders SwiftUI again, so it waits for
    /// the update to end.
    func setCompletionOpen(_ open: Bool) {
        guard open != completionOpen else { return }
        completionOpen = open
        DispatchQueue.main.async { [weak self] in
            guard let self, self.isFirstResponder else { return }
            self.reloadInputViews()
        }
    }

    // MARK: Keys

    private static let returnCommands: [UIKeyCommand] = [
        command("\r", [], #selector(handleReturn)),
        command("\r", .shift, #selector(handleShiftReturn)),
        command("\r", .command, #selector(handleCommandReturn)),
    ]

    override var keyCommands: [UIKeyCommand]? {
        (super.keyCommands ?? []) + Self.returnCommands + (completionOpen ? completionCommands : [])
    }

    private static func command(_ input: String, _ flags: UIKeyModifierFlags, _ action: Selector) -> UIKeyCommand {
        let command = UIKeyCommand(input: input, modifierFlags: flags, action: action)
        command.wantsPriorityOverSystemBehavior = true
        return command
    }

    @objc private func handleUp() { keyHandler?.moveCompletion(-1) }
    @objc private func handleDown() { keyHandler?.moveCompletion(1) }
    @objc private func handleTab() { keyHandler?.acceptCompletion() }
    @objc private func handleEscape() { keyHandler?.closeCompletion() }
    @objc private func handleReturn() { keyHandler?.handleReturn(shift: false, command: false) }
    @objc private func handleShiftReturn() { keyHandler?.handleReturn(shift: true, command: false) }
    @objc private func handleCommandReturn() { keyHandler?.handleReturn(shift: false, command: true) }

    // MARK: Paste

    override func paste(_ sender: Any?) {
        let pasteboard = UIPasteboard.general
        if pasteboard.hasImages {
            var images = pasteboard.images ?? []
            if images.isEmpty, let image = pasteboard.image { images = [image] }
            let data = images.compactMap { $0.pngData() }
            if !data.isEmpty {
                onPasteImages?(data)
                return
            }
        }
        if let string = pasteboard.string, !string.isEmpty {
            insertText(string)
        } else {
            super.paste(sender)
        }
    }

    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        if action == #selector(paste(_:)) {
            let pasteboard = UIPasteboard.general
            return pasteboard.hasImages || pasteboard.hasStrings
                || super.canPerformAction(action, withSender: sender)
        }
        return super.canPerformAction(action, withSender: sender)
    }

    // MARK: Style

    private var bodyFont: UIFont { UIFont.preferredFont(forTextStyle: .body) }

    private var textAttributes: [NSAttributedString.Key: Any] {
        [.font: bodyFont, .foregroundColor: UIColor.label]
    }

    private func chipAttachment(_ chip: Chip) -> NSTextAttachment {
        ChipRenderer.attachment(chip, font: bodyFont, trait: traitCollection)
    }

    /// Redraws the chips and text for a new appearance or text size.
    private func restyle() {
        let parts = Draft.parts(from: attributedText)
        let selection = selectedRange
        let content = Draft.attributedString(parts, attributes: textAttributes) { self.chipAttachment($0) }
        attributedText = content
        typingAttributes = textAttributes
        let length = (text as NSString).length
        selectedRange = NSRange(location: min(selection.location, length), length: 0)
        invalidateIntrinsicContentSize()
        setNeedsLayout()
    }
}

/// Wraps `ComposerTextView` for SwiftUI: binds the draft, reports its height
/// and forwards the completion keys.
struct ComposerTextEditor: UIViewRepresentable {
    let model: ComposerModel
    let parts: [DraftPart]
    let onSubmit: () -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(model: model, onSubmit: onSubmit)
    }

    func makeUIView(context: Context) -> ComposerTextView {
        let view = ComposerTextView()
        view.delegate = context.coordinator
        view.keyHandler = context.coordinator
        view.onPasteImages = { [weak model] data in
            MainActor.assumeIsolated {
                for image in data { model?.addImage(image, baseName: "Pasted image") }
            }
        }
        context.coordinator.textView = view
        model.editor = view
        context.coordinator.apply(parts, to: view)
        return view
    }

    func updateUIView(_ view: ComposerTextView, context: Context) {
        context.coordinator.onSubmit = onSubmit
        context.coordinator.apply(parts, to: view)
        view.setCompletionOpen(model.completionOpen)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: ComposerTextView, context: Context) -> CGSize? {
        guard let width = proposal.width else { return nil }
        return CGSize(width: width, height: uiView.measuredHeight(forWidth: width))
    }

    @MainActor
    final class Coordinator: NSObject, UITextViewDelegate, ComposerKeyHandling {
        let model: ComposerModel
        var onSubmit: () -> Void
        weak var textView: ComposerTextView?
        private var isApplying = false

        init(model: ComposerModel, onSubmit: @escaping () -> Void) {
            self.model = model
            self.onSubmit = onSubmit
        }

        func apply(_ parts: [DraftPart], to view: ComposerTextView) {
            guard parts != Draft.parts(from: view.attributedText) else { return }
            isApplying = true
            view.setParts(parts, cursorAtEnd: true)
            isApplying = false
        }

        func textViewDidChange(_ textView: UITextView) {
            guard !isApplying else { return }
            sync(textView)
        }

        func textViewDidChangeSelection(_ textView: UITextView) {
            guard !isApplying else { return }
            updateCompletion(textView)
        }

        private func sync(_ textView: UITextView) {
            guard let view = textView as? ComposerTextView else { return }
            let parts = view.draftParts
            if model.parts != parts { model.parts = parts }
            updateCompletion(view)
        }

        private func updateCompletion(_ textView: UITextView) {
            guard let view = textView as? ComposerTextView else { return }
            model.updateCompletion(text: view.draftText, cursor: view.cursorOffset)
        }

        func moveCompletion(_ delta: Int) {
            model.moveSelection(delta)
        }

        func acceptCompletion() {
            model.acceptSelected()
        }

        func closeCompletion() {
            model.closeCompletion()
        }

        func handleReturn(shift: Bool, command: Bool) {
            if command {
                onSubmit()
                return
            }
            if !shift, model.acceptSelected() { return }
            if !shift, model.returnSends {
                onSubmit()
                return
            }
            textView?.insertText("\n")
        }
    }
}
