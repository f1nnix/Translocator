import AppKit
import TLCore

/// The table of a pane. It keeps nothing of its own: every key, click,
/// drag and menu becomes core input, and what it draws comes back in the
/// next snapshot.
final class FileTableView: NSTableView {
    weak var coordinator: FileTableCoordinator?
    private var pressRow: Int?
    private var pressEvent: NSEvent?
    private var dragging = false

    private var side: Side? { coordinator?.side }
    private var store: AppStore? { coordinator?.store }

    // ----- focus ----------------------------------------------------------------

    override var acceptsFirstResponder: Bool { true }

    override func becomeFirstResponder() -> Bool {
        let ok = super.becomeFirstResponder()
        if ok { coordinator?.focusChanged() }
        return ok
    }

    override func resignFirstResponder() -> Bool {
        let ok = super.resignFirstResponder()
        if ok { DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.coordinator?.focusChanged() } } }
        return ok
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        // The active pane takes the keyboard when the window opens.
        guard window != nil, let store, let side, store.snapshot.active == side, store.focusedPane == nil else { return }
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.takeFocus() } }
    }

    func takeFocus() {
        window?.makeFirstResponder(self)
    }

    // ----- keys -----------------------------------------------------------------

    override func keyDown(with event: NSEvent) {
        guard let store, let side, let pane = coordinator?.pane else { return super.keyDown(with: event) }
        if event.keyCode == 53 {
            // Escape clears the filter; with none it is left alone.
            if !pane.filter.isEmpty { store.command(.clearFilter, on: side) }
            return
        }
        if let target = store.keys.target(for: event, in: [.pane, .workspace, .global]),
           case .command(let command) = target
        {
            store.command(fill(command), on: side)
            return
        }
        let flags = event.modifierFlags.intersection([.command, .control, .option])
        if flags.isEmpty, let text = event.characters, !text.isEmpty,
           !text.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) || $0.value >= 0xF700 })
        {
            // The type-ahead filter; with no filter yet, + and - ask for a mask.
            store.command(.type(text: text), on: side)
            return
        }
        NSSound.beep()
    }

    /// Page Up and Down move by what fits in the list.
    private func fill(_ command: Command) -> Command {
        switch command {
        case .pageUp: return .pageUp(rows: pageRows)
        case .pageDown: return .pageDown(rows: pageRows)
        default: return command
        }
    }

    private var pageRows: UInt32 {
        let visible = enclosingScrollView?.contentView.bounds.height ?? 400
        return UInt32(max(Int(visible / max(rowHeight, 1)) - 1, 1))
    }

    // The Edit menu's commands, through the responder chain.
    @objc func copy(_ sender: Any?) { store?.command(.clipboardCopy) }
    @objc func cut(_ sender: Any?) { store?.command(.clipboardCut) }
    @objc func paste(_ sender: Any?) { store?.command(.clipboardPaste) }
    override func selectAll(_ sender: Any?) { store?.command(.markAll) }

    // ----- mouse ----------------------------------------------------------------

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        let point = convert(event.locationInWindow, from: nil)
        let row = self.row(at: point)
        pressRow = nil
        guard row >= 0, let store, let side else { return }
        let flags = event.modifierFlags
        if event.clickCount == 2 {
            store.pane(side, .doubleClick(row: UInt32(row)))
            return
        }
        let toggle = flags.contains(.command)
        store.pane(side, .click(row: UInt32(row), toggle: toggle, range: !toggle && flags.contains(.shift)))
        pressRow = row
        pressEvent = event
        dragging = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard !dragging, let row = pressRow, let press = pressEvent else { return }
        let a = press.locationInWindow, b = event.locationInWindow
        guard hypot(a.x - b.x, a.y - b.y) > 4 else { return }
        dragging = true
        startDrag(row: row, event: event)
    }

    override func mouseUp(with event: NSEvent) {
        pressRow = nil
        pressEvent = nil
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        guard let store, let side, let pane = coordinator?.pane else { return nil }
        window?.makeFirstResponder(self)
        let row = self.row(at: convert(event.locationInWindow, from: nil))
        if row >= 0 { store.pane(side, .contextClick(row: UInt32(row))) }
        return ContextMenu.build(store: store, side: side, pane: pane)
    }

    // ----- dragging out -----------------------------------------------------------

    private func startDrag(row: Int, event: NSEvent) {
        guard let store, let side, let items = store.core.drag(side: side, row: UInt32(row)) else { return }
        guard let payload = try? JSONEncoder().encode(DragPayload(items.dragged)) else { return }
        let image = dragImage(items.label)
        let origin = convert(event.locationInWindow, from: nil)
        let frame = NSRect(x: origin.x - 8, y: origin.y - image.size.height / 2, width: image.size.width,
                           height: image.size.height)
        var dragItems: [NSDraggingItem] = []
        if items.localPaths.isEmpty {
            let item = NSPasteboardItem()
            item.setData(payload, forType: .translocatorFiles)
            dragItems.append(NSDraggingItem(pasteboardWriter: item))
        } else {
            // One item per file, so Finder and other apps take them all.
            for (ix, path) in items.localPaths.enumerated() {
                let item = NSPasteboardItem()
                item.setString(URL(fileURLWithPath: path).absoluteString, forType: .fileURL)
                if ix == 0 { item.setData(payload, forType: .translocatorFiles) }
                dragItems.append(NSDraggingItem(pasteboardWriter: item))
            }
        }
        for (ix, item) in dragItems.enumerated() {
            item.setDraggingFrame(frame.offsetBy(dx: CGFloat(ix) * 2, dy: CGFloat(-ix) * 2), contents: ix == 0 ? image : nil)
        }
        beginDraggingSession(with: dragItems, event: event, source: self)
    }

    private func dragImage(_ label: String) -> NSImage {
        let text = NSAttributedString(string: label, attributes: [
            .font: NSFont.systemFont(ofSize: 12, weight: .medium),
            .foregroundColor: NSColor.labelColor,
        ])
        let size = NSSize(width: min(text.size().width + 34, 320), height: 24)
        return NSImage(size: size, flipped: false) { rect in
            NSColor.windowBackgroundColor.withAlphaComponent(0.92).setFill()
            NSBezierPath(roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), xRadius: 6, yRadius: 6).fill()
            NSColor.separatorColor.setStroke()
            NSBezierPath(roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), xRadius: 6, yRadius: 6).stroke()
            symbolImage("doc.on.doc")?.draw(in: NSRect(x: 8, y: 5, width: 14, height: 14))
            text.draw(in: NSRect(x: 26, y: 4, width: rect.width - 30, height: 16))
            return true
        }
    }

    override func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        // Other applications copy; a drop on the other pane says itself.
        context == .outsideApplication ? .copy : [.copy, .move, .generic]
    }

    // ----- the keyboard locations menu ------------------------------------------------

    func popUpLocations() {
        guard let store, let side else { return }
        takeFocus()
        let menu = LocationMenu.build(store: store, side: side)
        let top = visibleRect.minY + 4
        menu.popUp(positioning: nil, at: NSPoint(x: 8, y: top), in: self)
    }
}
