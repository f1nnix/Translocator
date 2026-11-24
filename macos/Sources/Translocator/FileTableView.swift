import AppKit

/// The table of a pane. It keeps nothing of its own: every key, click,
/// drag and menu becomes core input, and what it draws comes back in the
/// next snapshot.
final class FileTableView: NSTableView, PaneFocusable {
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
        guard window != nil, let store, let side, store.snapshot.active == side,
              store.focusedPane == nil || store.focusedPane == side else { return }
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.takeFocus() } }
    }

    func takeFocus() {
        window?.makeFirstResponder(self)
    }

    // ----- keys -----------------------------------------------------------------

    override func keyDown(with event: NSEvent) {
        guard let store, let side, let pane = coordinator?.pane else { return super.keyDown(with: event) }
        if !PaneKeys.keyDown(event, store: store, side: side, pane: pane, page: pageRows) { NSSound.beep() }
    }

    /// Page Up and Down move by what fits in the list.
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
        guard let store, let side else { return }
        PaneDrag.start(from: self, store: store, side: side, row: row, event: event)
    }

    override func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        PaneDrag.mask(context)
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
