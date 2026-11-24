import AppKit
import SwiftUI

/// A pane in the icon view: the rows of the list, as icons wrapped to the
/// pane's width.
///
/// Like the list it keeps nothing of its own: keys and clicks go to the
/// core, and the cursor and marks come back in the snapshot. The core moves
/// the cursor by whole rows of icons, so it is told how many fit in one.
struct FileGrid: NSViewRepresentable {
    let side: Side
    let pane: PaneSnapshot
    let general: GeneralSettings
    let active: Bool
    /// Read here so that a change of colour scheme updates the icons.
    var colors = Theme.shared.colors
    @Environment(AppStore.self) private var store

    func makeCoordinator() -> FileGridCoordinator {
        FileGridCoordinator(side: side, store: store)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = context.coordinator.makeScroll()
        context.coordinator.apply(pane: pane, general: general, colors: colors)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.apply(pane: pane, general: general, colors: colors)
    }
}

@MainActor
final class FileGridCoordinator: NSObject, NSCollectionViewDataSource, NSCollectionViewDelegate {
    let side: Side
    unowned let store: AppStore
    weak var grid: FileGridView?
    private let layout = NSCollectionViewFlowLayout()
    private(set) var pane: PaneSnapshot?
    private var general: GeneralSettings?
    private var colors = ContentColors.system
    private var cache = RowCache()
    /// Icons per row, as last told to the core.
    private(set) var perRow = 0
    private var observers: [NSObjectProtocol] = []

    init(side: Side, store: AppStore) {
        self.side = side
        self.store = store
    }

    var focused: Bool {
        guard let grid, let window = grid.window else { return false }
        return window.firstResponder === grid && window.isKeyWindow
    }

    func makeScroll() -> NSScrollView {
        let grid = FileGridView()
        grid.coordinator = self
        self.grid = grid
        layout.minimumInteritemSpacing = 4
        layout.minimumLineSpacing = 6
        layout.sectionInset = NSEdgeInsets(top: 10, left: 10, bottom: 10, right: 10)
        grid.collectionViewLayout = layout
        grid.dataSource = self
        grid.delegate = self
        grid.isSelectable = false
        grid.register(GridItem.self, forItemWithIdentifier: GridItem.identifier)
        grid.registerForDraggedTypes(PaneDrop.types)
        grid.setDraggingSourceOperationMask([.copy], forLocal: false)
        grid.setAccessibilityLabel("\(side.name) pane files")
        let scroll = NSScrollView()
        scroll.documentView = grid
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.drawsBackground = true
        scroll.postsFrameChangedNotifications = true
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: NSView.frameDidChangeNotification, object: scroll, queue: .main) {
            [weak self] _ in MainActor.assumeIsolated { self?.updatePerRow() }
        })
        for name in [NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification] {
            observers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] note in
                let window = (note.object as AnyObject?).map(ObjectIdentifier.init)
                MainActor.assumeIsolated {
                    guard let self, let window, window == self.grid?.window.map(ObjectIdentifier.init) else { return }
                    self.focusChanged()
                }
            })
        }
        store.register(grid, for: side)
        return scroll
    }

    func apply(pane next: PaneSnapshot, general nextGeneral: GeneralSettings, colors nextColors: ContentColors) {
        guard let grid else { return }
        let old = pane
        pane = next
        var reload = old == nil || old!.listingVersion != next.listingVersion || old!.rowCount != next.rowCount
            || old!.tabId != next.tabId || colors != nextColors
        if general != nextGeneral {
            if general?.fontSize != nextGeneral.fontSize || general?.showIcons != nextGeneral.showIcons {
                reload = true
                let font = CGFloat(nextGeneral.fontSize)
                layout.itemSize = NSSize(width: max(92, font * 7.5), height: 48 + ceil(font * 1.25) * 2 + 10)
                perRow = 0
            }
            general = nextGeneral
        }
        colors = nextColors
        grid.backgroundColors = [nextColors.background]
        grid.enclosingScrollView?.backgroundColor = nextColors.background
        if reload {
            cache.clear()
            grid.reloadData()
        } else if let old, old.cursor != next.cursor {
            redraw([Int(old.cursor), Int(next.cursor)])
        }
        let cursor = Int(next.cursor)
        if cursor < Int(next.rowCount), reload || old?.cursor != next.cursor {
            grid.scrollToItems(at: [IndexPath(item: cursor, section: 0)], scrollPosition: .nearestHorizontalEdge)
        }
        updatePerRow()
    }

    /// Tells the core how many icons a row holds, when that changed.
    func updatePerRow() {
        guard let grid, let scroll = grid.enclosingScrollView else { return }
        let inset = layout.sectionInset.left + layout.sectionInset.right
        let width = scroll.contentSize.width - inset
        let item = layout.itemSize.width
        let n = max(1, Int((width + layout.minimumInteritemSpacing) / (item + layout.minimumInteritemSpacing)))
        guard n != perRow else { return }
        perRow = n
        let side = side
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated { self?.store.pane(side, .gridColumns(count: UInt32(n))) }
        }
    }

    /// What Page Up and Down move by: the rows of icons in view.
    var page: UInt32 {
        guard let grid, let scroll = grid.enclosingScrollView else { return 1 }
        let rows = Int(scroll.contentSize.height / (layout.itemSize.height + layout.minimumLineSpacing))
        return UInt32(max(rows - 1, 1) * max(perRow, 1))
    }

    func redraw(_ items: [Int]) {
        guard let grid, let pane else { return }
        for ix in items where ix >= 0 && ix < Int(pane.rowCount) {
            if let item = grid.item(at: IndexPath(item: ix, section: 0)) as? GridItem {
                configure(item, ix)
            }
        }
    }

    func focusChanged() {
        guard let pane else { return }
        redraw([Int(pane.cursor)])
        store.listFocusChanged(side, focused: focused)
    }

    func row(_ ix: Int) -> Row? {
        guard let pane else { return nil }
        let side = side
        return cache.row(ix, count: Int(pane.rowCount)) { start, count in
            store.core.rows(side: side, start: start, count: count)
        }
    }

    private func configure(_ item: GridItem, _ ix: Int) {
        guard let row = row(ix), let pane, let general else { return }
        let cursor = Int(pane.cursor) == ix
        let style = RowStyle(row: row, cursor: cursor, focused: cursor && focused, fontSize: CGFloat(general.fontSize),
                             colors: colors)
        item.show(row, style: style, cursor: cursor)
    }

    func numberOfSections(in collectionView: NSCollectionView) -> Int { 1 }

    func collectionView(_ collectionView: NSCollectionView, numberOfItemsInSection section: Int) -> Int {
        Int(pane?.rowCount ?? 0)
    }

    func collectionView(_ collectionView: NSCollectionView, itemForRepresentedObjectAt indexPath: IndexPath) -> NSCollectionViewItem {
        let item = collectionView.makeItem(withIdentifier: GridItem.identifier, for: indexPath)
        if let item = item as? GridItem { configure(item, indexPath.item) }
        return item
    }

    // ----- drops ------------------------------------------------------------

    func collectionView(
        _ collectionView: NSCollectionView, validateDrop draggingInfo: NSDraggingInfo,
        proposedIndexPath proposedDropIndexPath: AutoreleasingUnsafeMutablePointer<NSIndexPath>,
        dropOperation proposedDropOperation: UnsafeMutablePointer<NSCollectionView.DropOperation>
    ) -> NSDragOperation {
        PaneDrop.validate(draggingInfo, side: side)
    }

    func collectionView(
        _ collectionView: NSCollectionView, acceptDrop draggingInfo: NSDraggingInfo, indexPath: IndexPath,
        dropOperation: NSCollectionView.DropOperation
    ) -> Bool {
        PaneDrop.accept(draggingInfo, side: side, store: store)
    }
}

/// The grid of a pane. Every key, click, drag and menu becomes core input.
final class FileGridView: NSCollectionView, PaneFocusable {
    weak var coordinator: FileGridCoordinator?
    private var pressItem: Int?
    private var pressEvent: NSEvent?
    private var dragging = false

    private var side: Side? { coordinator?.side }
    private var store: AppStore? { coordinator?.store }

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
        guard window != nil, let store, let side, store.snapshot.active == side,
              store.focusedPane == nil || store.focusedPane == side else { return }
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.takeFocus() } }
    }

    func takeFocus() {
        window?.makeFirstResponder(self)
    }

    override func keyDown(with event: NSEvent) {
        guard let store, let side, let coordinator, let pane = coordinator.pane else { return super.keyDown(with: event) }
        if !PaneKeys.keyDown(event, store: store, side: side, pane: pane, page: coordinator.page) { NSSound.beep() }
    }

    @objc func copy(_ sender: Any?) { store?.command(.clipboardCopy) }
    @objc func cut(_ sender: Any?) { store?.command(.clipboardCut) }
    @objc func paste(_ sender: Any?) { store?.command(.clipboardPaste) }
    override func selectAll(_ sender: Any?) { store?.command(.markAll) }

    private func item(at event: NSEvent) -> Int? {
        indexPathForItem(at: convert(event.locationInWindow, from: nil))?.item
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        pressItem = nil
        guard let ix = item(at: event), let store, let side else { return }
        if event.clickCount == 2 {
            store.pane(side, .doubleClick(row: UInt32(ix)))
            return
        }
        let flags = event.modifierFlags
        let toggle = flags.contains(.command)
        store.pane(side, .click(row: UInt32(ix), toggle: toggle, range: !toggle && flags.contains(.shift)))
        pressItem = ix
        pressEvent = event
        dragging = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard !dragging, let ix = pressItem, let press = pressEvent, let store, let side else { return }
        let a = press.locationInWindow, b = event.locationInWindow
        guard hypot(a.x - b.x, a.y - b.y) > 4 else { return }
        dragging = true
        PaneDrag.start(from: self, store: store, side: side, row: ix, event: event)
    }

    override func mouseUp(with event: NSEvent) {
        pressItem = nil
        pressEvent = nil
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        guard let store, let side, let pane = coordinator?.pane else { return nil }
        window?.makeFirstResponder(self)
        if let ix = item(at: event) { store.pane(side, .contextClick(row: UInt32(ix))) }
        return ContextMenu.build(store: store, side: side, pane: pane)
    }

    override func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        PaneDrag.mask(context)
    }

    func popUpLocations() {
        guard let store, let side else { return }
        takeFocus()
        let menu = LocationMenu.build(store: store, side: side)
        menu.popUp(positioning: nil, at: NSPoint(x: 8, y: visibleRect.maxY - 4), in: self)
    }
}

/// One icon of the grid: the symbol on a rounded plate, the name under it.
final class GridItem: NSCollectionViewItem {
    static let identifier = NSUserInterfaceItemIdentifier("GridItem")
    private let plate = GridPlate()
    private let icon = NSImageView()
    private let name = NSTextField(wrappingLabelWithString: "")

    override func loadView() {
        view = plate
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.imageScaling = .scaleProportionallyUpOrDown
        name.translatesAutoresizingMaskIntoConstraints = false
        name.alignment = .center
        name.maximumNumberOfLines = 2
        name.lineBreakMode = .byTruncatingMiddle
        name.cell?.truncatesLastVisibleLine = true
        name.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        plate.addSubview(icon)
        plate.addSubview(name)
        NSLayoutConstraint.activate([
            icon.topAnchor.constraint(equalTo: plate.topAnchor, constant: 6),
            icon.centerXAnchor.constraint(equalTo: plate.centerXAnchor),
            icon.widthAnchor.constraint(equalToConstant: 40),
            icon.heightAnchor.constraint(equalToConstant: 36),
            name.topAnchor.constraint(equalTo: icon.bottomAnchor, constant: 6),
            name.leadingAnchor.constraint(equalTo: plate.leadingAnchor, constant: 3),
            name.trailingAnchor.constraint(equalTo: plate.trailingAnchor, constant: -3),
        ])
    }

    func show(_ row: Row, style: RowStyle, cursor: Bool) {
        name.stringValue = row.name
        name.font = style.font
        name.textColor = style.text
        let config = NSImage.SymbolConfiguration(pointSize: 30, weight: .regular)
        icon.image = symbolImage(row.icon.symbol)?.withSymbolConfiguration(config)
        icon.contentTintColor = style.icon
        plate.fill =
            if cursor && style.focused { style.colors.cursor }
            else if cursor { style.colors.cursorInactive }
            else if row.marked { style.colors.markedBackground }
            else { nil }
        plate.alphaValue = style.alpha
        var description = row.isParent ? "Enclosing folder" : row.name
        if row.isDir && !row.isParent { description += ", folder" }
        if row.marked { description += ", marked" }
        plate.setAccessibilityElement(true)
        plate.setAccessibilityRole(.cell)
        plate.setAccessibilityLabel(description)
        plate.setAccessibilitySelected(cursor)
    }
}

final class GridPlate: NSView {
    var fill: NSColor? { didSet { needsDisplay = true } }

    override func draw(_ dirtyRect: NSRect) {
        guard let fill else { return }
        fill.setFill()
        NSBezierPath(roundedRect: bounds.insetBy(dx: 1, dy: 1), xRadius: 8, yRadius: 8).fill()
    }

    // Clicks belong to the grid, which turns them into core input.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}
