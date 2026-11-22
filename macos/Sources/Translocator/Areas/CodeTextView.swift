import AppKit
import SwiftUI
import TLCore

/// The text view of the viewer, the editor and quick look: an `NSTextView`
/// (TextKit 1, for the line-number ruler) in a monospaced font, with no
/// wrapping, the find bar (⌘F, ⌘G, ⇧⌘G, ⌘E, ⌥⌘F), undo, and syntax colours
/// from the core's highlighter (`TLCore.highlight`).
///
/// The text is loaded whenever `revision` changes (the editor's
/// `content_version`, quick look's `revision`) and nothing else replaces
/// it: after that the text view owns the live text. Counting lines and
/// colouring run off the main thread, again a moment after each edit; the
/// colours are drawn through the layout manager's temporary attributes, so
/// they cost nothing until a line is on screen and never touch the text or
/// its undo.
struct CodeView: NSViewRepresentable {
    /// The text is loaded again when this changes.
    let revision: UInt64
    /// The text to load; asked for only when `revision` changed.
    let text: () -> String
    /// As `language_for` names it (`rust`, `text`).
    let language: String
    let editable: Bool
    let palette: SyntaxPalette
    /// Filled with the controller, for reading the text back (Save).
    var handle: CodeHandle?
    /// Every edit the user makes.
    var onEdit: (() -> Void)?
    /// The number of lines, after a load and a moment after edits.
    var onLines: ((Int) -> Void)?
    /// Takes the keyboard once the first text is in (the editor does; quick
    /// look does not: the file list keeps the keyboard there).
    var takesFocus = false

    func makeCoordinator() -> CodeController { CodeController() }

    func makeNSView(context: Context) -> NSScrollView { context.coordinator.scrollView }

    func updateNSView(_ view: NSScrollView, context: Context) {
        let controller = context.coordinator
        controller.onEdit = onEdit
        controller.onLines = onLines
        handle?.controller = controller
        controller.setPalette(palette)
        controller.setEditable(editable)
        if controller.revision != revision {
            let first = controller.revision == nil
            controller.load(text(), language: language, revision: revision)
            if first && takesFocus {
                // The view joins its window after this update.
                let textView = controller.textView
                DispatchQueue.main.async { textView.window?.makeFirstResponder(textView) }
            }
        }
    }

    static func dismantleNSView(_ view: NSScrollView, coordinator: CodeController) {
        coordinator.stop()
    }
}

/// Lets a view reach the text of its `CodeView`.
@MainActor
final class CodeHandle {
    weak var controller: CodeController?

    /// The text as the user has it now.
    var text: String { controller?.text ?? "" }
}

// MARK: - colours

/// What each kind of highlighted text is drawn in, with the text view's
/// own colours. Built from the content scheme (`Theme`): a scheme's ANSI
/// colours, or for the system's own scheme an Xcode-like palette that
/// follows the appearance by itself.
struct SyntaxPalette: Equatable {
    var text: NSColor
    var background: NSColor
    /// The ruler's numbers, background and edge.
    var lineNumber: NSColor
    var gutter: NSColor
    var gutterEdge: NSColor
    var kinds: [HighlightKind: NSColor]

    func color(_ kind: HighlightKind) -> NSColor { kinds[kind] ?? text }

    init(_ c: ContentColors) {
        text = c.text
        background = c.background
        lineNumber = c.secondary.withAlphaComponent(0.8)
        gutter = c.background
        gutterEdge = c.border
        if c.isSystem {
            kinds = SyntaxPalette.xcode
        } else {
            kinds = [
                .keyword: c.magenta, .string: c.green, .escape: c.cyan, .number: c.yellow,
                .constant: c.yellow, .comment: c.secondary, .function: c.blue, .type: c.cyan,
                .property: c.red, .punctuation: c.text.withAlphaComponent(0.7), .operator: c.text,
                .tag: c.red, .attribute: c.yellow, .heading: c.blue, .link: c.cyan, .literal: c.green,
                .inserted: c.green, .deleted: c.red,
            ]
        }
    }

    /// Xcode's default light and dark colours, resolved at draw time.
    private static let xcode: [HighlightKind: NSColor] = {
        func pair(_ light: UInt32, _ dark: UInt32) -> NSColor {
            NSColor(name: nil) { appearance in
                let hex = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? dark : light
                return NSColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
                               blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
            }
        }
        return [
            .keyword: pair(0x9B2393, 0xFF7AB2), .string: pair(0xC41A16, 0xFF8170),
            .escape: pair(0xB5520F, 0xFFA14F), .number: pair(0x1C00CF, 0xD9C97C),
            .constant: pair(0x6C36A9, 0xB281EB), .comment: pair(0x5D6C79, 0x7F8C98),
            .function: pair(0x326D74, 0x67B7A4), .type: pair(0x3900A0, 0xDABAFF),
            .property: pair(0x0F68A0, 0x4EB0CC), .punctuation: .secondaryLabelColor,
            .operator: .labelColor, .tag: pair(0x0B4F79, 0x5DD8FF),
            .attribute: pair(0x815F03, 0xBF8555), .heading: pair(0x0B4F79, 0x5DD8FF),
            .link: .linkColor, .literal: pair(0xC41A16, 0xFF8170),
            .inserted: .systemGreen, .deleted: .systemRed,
        ]
    }()
}

// MARK: - the controller

/// Owns the scroll view, text view and ruler of one `CodeView`.
@MainActor
final class CodeController: NSObject, NSTextViewDelegate, NSTextStorageDelegate, NSLayoutManagerDelegate {
    let scrollView = NSScrollView()
    let textView: CodeNSTextView
    private let ruler: LineNumberRuler

    /// What was loaded last; nil before the first load.
    private(set) var revision: UInt64?
    private var language = "text"
    private var palette: SyntaxPalette?
    var onEdit: (() -> Void)?
    var onLines: ((Int) -> Void)?

    /// The colours, as last computed, in the text's offsets as it was then.
    /// Read by the layout manager while it draws (on the main thread).
    nonisolated(unsafe) private var spans: [HighlightSpan] = []
    /// Edits since `spans` were computed, oldest first: drawing maps back
    /// through them until the next colouring.
    nonisolated(unsafe) private var edits: [(at: Int, removed: Int, inserted: Int)] = []
    nonisolated(unsafe) private var colors: [HighlightKind: NSColor] = [:]
    /// Bumped by every load and edit: older results are dropped.
    private var generation = 0
    /// Set while `load` replaces the text: that is no edit.
    private var loading = false
    private var pending: Task<Void, Never>?
    private var scrollObserver: NSObjectProtocol?

    static let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
    private static let marker = NSAttributedString.Key("TLHighlighted")

    var text: String { textView.string }

    override init() {
        let storage = NSTextStorage()
        let layout = NSLayoutManager()
        layout.allowsNonContiguousLayout = true
        storage.addLayoutManager(layout)
        let container = NSTextContainer(size: NSSize(width: CGFloat.greatestFiniteMagnitude,
                                                     height: CGFloat.greatestFiniteMagnitude))
        container.widthTracksTextView = false
        layout.addTextContainer(container)
        textView = CodeNSTextView(frame: NSRect(x: 0, y: 0, width: 400, height: 300), textContainer: container)
        ruler = LineNumberRuler(scrollView: scrollView, orientation: .verticalRuler)
        super.init()

        let tv = textView
        tv.minSize = .zero
        tv.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        tv.isVerticallyResizable = true
        tv.isHorizontallyResizable = true
        tv.autoresizingMask = [.width, .height]
        tv.textContainerInset = NSSize(width: 2, height: 6)
        tv.isRichText = false
        tv.importsGraphics = false
        tv.usesFontPanel = false
        tv.allowsUndo = true
        tv.usesFindBar = true
        tv.isIncrementalSearchingEnabled = true
        tv.isAutomaticQuoteSubstitutionEnabled = false
        tv.isAutomaticDashSubstitutionEnabled = false
        tv.isAutomaticTextReplacementEnabled = false
        tv.isAutomaticSpellingCorrectionEnabled = false
        tv.isContinuousSpellCheckingEnabled = false
        tv.isGrammarCheckingEnabled = false
        tv.isAutomaticLinkDetectionEnabled = false
        tv.isAutomaticDataDetectionEnabled = false
        tv.smartInsertDeleteEnabled = false
        tv.font = CodeController.font
        let paragraph = NSMutableParagraphStyle()
        paragraph.tabStops = []
        paragraph.defaultTabInterval = 4 * (" " as NSString).size(withAttributes: [.font: CodeController.font]).width
        tv.defaultParagraphStyle = paragraph
        tv.typingAttributes[.paragraphStyle] = paragraph
        tv.setAccessibilityLabel("Text")
        tv.delegate = self
        storage.delegate = self
        layout.delegate = self

        scrollView.documentView = tv
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        scrollView.findBarPosition = .aboveContent
        ruler.textView = tv
        ruler.clientView = tv
        scrollView.verticalRulerView = ruler
        scrollView.hasVerticalRuler = true
        scrollView.rulersVisible = true

        scrollView.contentView.postsBoundsChangedNotifications = true
        scrollObserver = NotificationCenter.default.addObserver(
            forName: NSView.boundsDidChangeNotification, object: scrollView.contentView, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.ruler.needsDisplay = true }
        }
    }

    func stop() {
        pending?.cancel()
        generation += 1
        if let scrollObserver { NotificationCenter.default.removeObserver(scrollObserver) }
        scrollObserver = nil
    }

    // ----- loading -------------------------------------------------------

    /// Replaces the text (and forgets its undo), then counts and colours it.
    func load(_ text: String, language: String, revision: UInt64) {
        self.revision = revision
        self.language = language
        pending?.cancel()
        loading = true
        textView.string = text
        if let storage = textView.textStorage {
            let all = NSRange(location: 0, length: storage.length)
            storage.beginEditing()
            storage.setAttributes([
                .font: CodeController.font,
                .foregroundColor: palette?.text ?? NSColor.textColor,
                .paragraphStyle: textView.defaultParagraphStyle ?? NSParagraphStyle.default,
            ], range: all)
            storage.endEditing()
        }
        spans = []
        edits = []
        textView.setSelectedRange(NSRange(location: 0, length: 0))
        textView.scroll(.zero)
        if textView.isEditable { textView.undoManager?.removeAllActions() }
        loading = false
        ruler.reset()
        refresh(after: nil)
    }

    func setEditable(_ editable: Bool) {
        if textView.isEditable != editable { textView.isEditable = editable }
        textView.isSelectable = true
    }

    func setPalette(_ next: SyntaxPalette) {
        guard next != palette else { return }
        palette = next
        colors = next.kinds
        textView.backgroundColor = next.background
        textView.insertionPointColor = next.text
        textView.textColor = next.text
        textView.typingAttributes[.foregroundColor] = next.text
        scrollView.backgroundColor = next.background
        ruler.palette = next
        if let storage = textView.textStorage {
            textView.layoutManager?.invalidateDisplay(forCharacterRange: NSRange(location: 0, length: storage.length))
        }
        textView.needsDisplay = true
    }

    /// Counts the lines and colours the text off the main thread, now or a
    /// moment after the last edit. The count is reported as soon as it is
    /// known, without waiting for the colours, which take longer.
    private func refresh(after delay: Duration?) {
        generation += 1
        let generation = generation
        let language = language
        pending?.cancel()
        pending = Task { [weak self] in
            if let delay {
                try? await Task.sleep(for: delay)
                if Task.isCancelled { return }
            }
            // Copied once the typing has paused, not on every keystroke.
            guard let text = self?.textView.string else { return }
            let lines = await Task.detached(priority: .userInitiated) {
                CodeController.countLines(text)
            }.value
            guard let current = self, !Task.isCancelled, current.generation == generation else { return }
            current.ruler.lineCount = lines
            current.onLines?(lines)
            let spans = language == "text" ? [] : await Task.detached(priority: .userInitiated) {
                highlight(text: text, language: language)
            }.value
            guard let self, !Task.isCancelled, self.generation == generation else { return }
            self.spans = spans
            self.edits = []
            if let storage = self.textView.textStorage, let layout = self.textView.layoutManager {
                let all = NSRange(location: 0, length: storage.length)
                // A temporary attribute over the whole text, so the layout
                // manager asks its delegate for the colours wherever it draws.
                layout.addTemporaryAttribute(CodeController.marker, value: true, forCharacterRange: all)
                layout.invalidateDisplay(forCharacterRange: all)
            }
        }
    }

    /// `1 +` the line feeds, as the GPUI shell's editor counts.
    nonisolated static func countLines(_ text: String) -> Int {
        var text = text
        return text.withUTF8 { buffer in
            guard var at = buffer.baseAddress else { return 1 }
            var left = buffer.count
            var lines = 1
            while left > 0, let hit = memchr(at, 0x0A, left) {
                let step = UnsafeRawPointer(hit) - UnsafeRawPointer(at) + 1
                lines += 1
                at += step
                left -= step
            }
            return lines
        }
    }

    // ----- edits -----------------------------------------------------------

    /// An edit by the user, through the text view.
    func textDidChange(_ notification: Notification) {
        onEdit?()
    }

    /// Every change of the characters, whoever makes it (typing, undo,
    /// the find bar's Replace All, an Accessibility client setting the
    /// value): the line count and the colours follow a moment later.
    nonisolated func textStorage(_ textStorage: NSTextStorage, didProcessEditing editedMask: NSTextStorageEditActions,
                                 range editedRange: NSRange, changeInLength delta: Int) {
        guard editedMask.contains(.editedCharacters) else { return }
        let edit = (at: editedRange.location, removed: editedRange.length - delta, inserted: editedRange.length)
        MainActor.assumeIsolated {
            edits.append(edit)
            ruler.textChanged(at: edit.at)
            if !loading { refresh(after: .milliseconds(300)) }
        }
    }

    // ----- drawing the colours -----------------------------------------------

    nonisolated func layoutManager(_ layoutManager: NSLayoutManager,
                                   shouldUseTemporaryAttributes attrs: [NSAttributedString.Key: Any] = [:],
                                   forDrawingToScreen toScreen: Bool, atCharacterIndex charIndex: Int,
                                   effectiveRange effectiveCharRange: NSRangePointer?) -> [NSAttributedString.Key: Any]? {
        guard toScreen, !spans.isEmpty else { return attrs }
        // Where this character was when the colours were computed.
        var index = charIndex
        for edit in edits.reversed() {
            if index < edit.at { continue }
            if index < edit.at + edit.inserted {
                effectiveCharRange?.pointee = NSRange(location: charIndex, length: 1)
                return attrs
            }
            index += edit.removed - edit.inserted
        }
        // The span at or after it.
        var low = 0
        var high = spans.count
        while low < high {
            let mid = (low + high) / 2
            if Int(spans[mid].startUtf16) + Int(spans[mid].lenUtf16) <= index { low = mid + 1 } else { high = mid }
        }
        var result = attrs
        let length: Int
        if low < spans.count, Int(spans[low].startUtf16) <= index {
            let span = spans[low]
            result[.foregroundColor] = colors[span.kind]
            length = Int(span.startUtf16) + Int(span.lenUtf16) - index
        } else if low < spans.count {
            length = Int(spans[low].startUtf16) - index
        } else {
            length = Int.max / 2
        }
        // With edits pending the runs are mapped one character at a time.
        let run = edits.isEmpty ? max(1, length) : 1
        if let effectiveCharRange {
            let limit = effectiveCharRange.pointee
            let end = min(charIndex + run, limit.length > 0 ? NSMaxRange(limit) : charIndex + run)
            effectiveCharRange.pointee = NSRange(location: charIndex, length: max(1, end - charIndex))
        }
        return result
    }
}

// MARK: - the text view

/// The find bar's keys while the text has the keyboard, whatever the
/// menus hold.
final class CodeNSTextView: NSTextView {
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if window?.firstResponder === self, let action = findAction(for: event) {
            let sender = NSMenuItem()
            sender.tag = action.rawValue
            performTextFinderAction(sender)
            return true
        }
        return super.performKeyEquivalent(with: event)
    }

    private func findAction(for event: NSEvent) -> NSTextFinder.Action? {
        let flags = event.modifierFlags.intersection([.command, .shift, .option, .control])
        switch (flags, event.charactersIgnoringModifiers?.lowercased()) {
        case ([.command], "f"): return .showFindInterface
        case ([.command, .option], "f"): return isEditable ? .showReplaceInterface : .showFindInterface
        case ([.command], "g"): return .nextMatch
        case ([.command, .shift], "g"): return .previousMatch
        case ([.command], "e"): return .setSearchString
        default: return nil
        }
    }
}

// MARK: - line numbers

/// Line numbers beside the text. The number of the first visible line is
/// counted from the last one drawn, so scrolling and typing below it stay
/// cheap however long the text is.
final class LineNumberRuler: NSRulerView {
    weak var textView: NSTextView?
    var palette: SyntaxPalette? { didSet { needsDisplay = true } }
    /// Sets the width: room for this many digits.
    var lineCount = 1 {
        didSet {
            let digits = max(3, String(lineCount).count)
            let width = ceil(CGFloat(digits) * digitWidth + 14)
            if abs(ruleThickness - width) > 0.5 { ruleThickness = width }
            needsDisplay = true
        }
    }

    private let font = NSFont.monospacedDigitSystemFont(ofSize: 10.5, weight: .regular)
    private lazy var digitWidth = ("8" as NSString).size(withAttributes: [.font: font]).width
    /// A character index and the number of the line it is on.
    private var anchor = (index: 0, line: 1)

    override init(scrollView: NSScrollView?, orientation: NSRulerView.Orientation) {
        super.init(scrollView: scrollView, orientation: orientation)
        clipsToBounds = true
        ruleThickness = 36
    }

    required init(coder: NSCoder) { fatalError("not used") }

    override var isOpaque: Bool { true }
    override var requiredThickness: CGFloat { ruleThickness }

    func reset() {
        anchor = (0, 1)
        needsDisplay = true
    }

    /// An edit before the anchor changes the lines above it.
    func textChanged(at location: Int) {
        if location < anchor.index { anchor = (0, 1) }
        needsDisplay = true
    }

    private func line(at index: Int, in string: NSString) -> Int {
        if index < anchor.index || anchor.index > string.length { anchor = (0, 1) }
        let line = anchor.line + LineNumberRuler.lineFeeds(in: string, from: anchor.index, to: index)
        anchor = (index, line)
        return line
    }

    private static func lineFeeds(in string: NSString, from start: Int, to end: Int) -> Int {
        guard end > start else { return 0 }
        let chunk = 16 * 1024
        var buffer = [unichar](repeating: 0, count: chunk)
        var count = 0
        var at = start
        buffer.withUnsafeMutableBufferPointer { pointer in
            guard let base = pointer.baseAddress else { return }
            while at < end {
                let n = min(chunk, end - at)
                string.getCharacters(base, range: NSRange(location: at, length: n))
                for k in 0..<n where base[k] == 0x0A { count += 1 }
                at += n
            }
        }
        return count
    }

    override func draw(_ dirtyRect: NSRect) {
        let palette = palette
        (palette?.gutter ?? .textBackgroundColor).setFill()
        dirtyRect.fill()
        (palette?.gutterEdge ?? .separatorColor).setFill()
        NSRect(x: bounds.maxX - 1, y: dirtyRect.minY, width: 1, height: dirtyRect.height).fill()

        guard let textView, let layout = textView.layoutManager, let container = textView.textContainer,
              let storage = textView.textStorage else { return }
        let string = storage.mutableString
        let attributes: [NSAttributedString.Key: Any] = [
            .font: font, .foregroundColor: palette?.lineNumber ?? NSColor.secondaryLabelColor,
        ]
        let origin = textView.textContainerOrigin
        func number(_ line: Int, top: CGFloat, height: CGFloat) {
            let label = String(line) as NSString
            let size = label.size(withAttributes: attributes)
            let y = convert(NSPoint(x: 0, y: top + origin.y), from: textView).y
            label.draw(at: NSPoint(x: ruleThickness - size.width - 7, y: y + (height - size.height) / 2),
                       withAttributes: attributes)
        }

        let length = string.length
        if length > 0 {
            let glyphs = layout.glyphRange(forBoundingRect: textView.visibleRect, in: container)
            let chars = layout.characterRange(forGlyphRange: glyphs, actualGlyphRange: nil)
            var index = string.lineRange(for: NSRange(location: min(chars.location, length - 1), length: 0)).location
            var line = self.line(at: index, in: string)
            let end = NSMaxRange(chars)
            while index < length && index <= end {
                let fragment = layout.lineFragmentRect(forGlyphAt: layout.glyphIndexForCharacter(at: index),
                                                       effectiveRange: nil)
                number(line, top: fragment.minY, height: fragment.height)
                let next = NSMaxRange(string.lineRange(for: NSRange(location: index, length: 0)))
                if next <= index { break }
                index = next
                line += 1
            }
            if index >= length, string.character(at: length - 1) == 0x0A {
                let extra = layout.extraLineFragmentRect
                if extra.height > 0 { number(line, top: extra.minY, height: extra.height) }
            }
        } else {
            let extra = layout.extraLineFragmentRect
            number(1, top: extra.minY, height: extra.height > 0 ? extra.height : CodeController.font.boundingRectForFont.height)
        }
    }
}
