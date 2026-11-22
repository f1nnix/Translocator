import AppKit
import SwiftUI
import TLCore

/// The drag payload between panes: what `PaneInput.drop` needs.
struct DragPayload: Codable {
    var side: String
    var conn: UInt64
    var dir: Data
    var names: [Data]
    var elevated: Bool

    init(_ d: Dragged) {
        side = d.side == .left ? "left" : "right"
        conn = d.conn
        dir = d.dir
        names = d.names
        elevated = d.elevated
    }

    var dragged: Dragged {
        Dragged(side: side == "left" ? .left : .right, conn: conn, dir: dir, names: names, elevated: elevated)
    }
}

extension NSPasteboard.PasteboardType {
    static let translocatorFiles = NSPasteboard.PasteboardType("dev.translocator.files")
}

/// A pane's file list: an AppKit table over the core's rows.
///
/// The table never owns a selection or a cursor of its own: clicks and
/// keys go to the core, and the rows are drawn from `PaneSnapshot.cursor`
/// and the rows fetched by range, which are cached until the pane's
/// `listing_version` moves.
struct FileTable: NSViewRepresentable {
    let side: Side
    let pane: PaneSnapshot
    let general: GeneralSettings
    let active: Bool
    /// Read here so that a change of colour scheme updates the list.
    var colors = Theme.shared.colors
    @Environment(AppStore.self) private var store

    func makeCoordinator() -> FileTableCoordinator {
        FileTableCoordinator(side: side, store: store)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let coordinator = context.coordinator
        let table = FileTableView()
        table.coordinator = coordinator
        coordinator.table = table
        table.dataSource = coordinator
        table.delegate = coordinator
        table.headerView = NSTableHeaderView()
        table.style = .fullWidth
        table.selectionHighlightStyle = .none
        table.allowsMultipleSelection = false
        table.allowsEmptySelection = true
        table.allowsColumnReordering = false
        table.usesAlternatingRowBackgroundColors = false
        table.intercellSpacing = NSSize(width: 6, height: 0)
        table.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        table.registerForDraggedTypes([.translocatorFiles, .fileURL])
        table.setDraggingSourceOperationMask([.copy], forLocal: false)
        table.setAccessibilityLabel("\(side.name) pane files")
        for column in FileColumn.allCases {
            let col = NSTableColumn(identifier: column.identifier)
            col.title = column.title
            col.headerCell.alignment = column.alignment
            col.width = column.width
            col.minWidth = column == .name ? 120 : 30
            col.resizingMask = column == .name ? .autoresizingMask : .userResizingMask
            table.addTableColumn(col)
        }
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.autohidesScrollers = true
        scroll.drawsBackground = true
        scroll.borderType = .noBorder
        scroll.postsFrameChangedNotifications = true
        coordinator.observeWidth(of: scroll)
        store.register(table, for: side)
        coordinator.apply(colors: colors)
        coordinator.apply(pane: pane, general: general, active: active)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.apply(colors: colors)
        context.coordinator.apply(pane: pane, general: general, active: active)
    }
}

enum FileColumn: String, CaseIterable {
    case name, ext, size, date, mode

    var identifier: NSUserInterfaceItemIdentifier { NSUserInterfaceItemIdentifier(rawValue) }

    var title: String {
        switch self {
        case .name: "Name"
        case .ext: "Ext"
        case .size: "Size"
        case .date: "Modified"
        case .mode: "Permissions"
        }
    }

    var width: CGFloat {
        switch self {
        case .name: 240
        case .ext: 52
        case .size: 74
        case .date: 116
        case .mode: 84
        }
    }

    var alignment: NSTextAlignment {
        switch self {
        case .size, .date: .right
        default: .left
        }
    }

    var sortKey: SortKey? {
        switch self {
        case .name: .name
        case .ext: .ext
        case .size: .size
        case .date: .modified
        case .mode: nil
        }
    }

    init?(_ identifier: NSUserInterfaceItemIdentifier?) {
        guard let raw = identifier?.rawValue, let column = FileColumn(rawValue: raw) else { return nil }
        self = column
    }
}

@MainActor
final class FileTableCoordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    let side: Side
    unowned let store: AppStore
    weak var table: FileTableView?

    private(set) var pane: PaneSnapshot?
    private var general: GeneralSettings?
    private var colors: ContentColors?
    private var cache: [Int: Row] = [:]
    private static let page = 64
    private var narrow = -1
    private var keyObservers: [NSObjectProtocol] = []

    init(side: Side, store: AppStore) {
        self.side = side
        self.store = store
    }

    /// The list has the keyboard, in the key window.
    var focused: Bool {
        guard let table, let window = table.window else { return false }
        return window.firstResponder === table && window.isKeyWindow
    }

    // ----- taking in a snapshot -----------------------------------------------

    /// The colour scheme (`Theme`): the canvas, and every row redrawn.
    func apply(colors next: ContentColors) {
        guard let table, next != colors else { return }
        let first = colors == nil
        colors = next
        table.backgroundColor = next.background
        table.enclosingScrollView?.backgroundColor = next.background
        if !first { table.reloadData() }
    }

    func apply(pane next: PaneSnapshot, general nextGeneral: GeneralSettings, active: Bool) {
        guard let table else { return }
        let old = pane
        pane = next
        var reload = old == nil || old!.listingVersion != next.listingVersion || old!.rowCount != next.rowCount
            || old!.tabId != next.tabId
        if general != nextGeneral {
            if general?.rowHeight != nextGeneral.rowHeight {
                table.rowHeight = CGFloat(nextGeneral.rowHeight)
            }
            if general?.fontSize != nextGeneral.fontSize || general?.showIcons != nextGeneral.showIcons {
                reload = true
                fitColumns(fontSize: CGFloat(nextGeneral.fontSize))
            }
            general = nextGeneral
        }
        if old?.sortKey != next.sortKey || old?.sortDescending != next.sortDescending || old == nil {
            updateSortIndicator()
        }
        if reload {
            cache.removeAll(keepingCapacity: true)
            table.reloadData()
        } else if let old, old.cursor != next.cursor {
            redraw(rows: [Int(old.cursor), Int(next.cursor)])
        }
        let cursor = Int(next.cursor)
        if cursor < Int(next.rowCount), reload || old?.cursor != next.cursor {
            table.scrollRowToVisible(cursor)
            // VoiceOver follows the selection; the cursor is drawn by the rows.
            table.selectRowIndexes(IndexSet(integer: cursor), byExtendingSelection: false)
        }
    }

    /// Updates rows in place (the cursor moved, or the focus changed).
    func redraw(rows: [Int]) {
        guard let table, let pane else { return }
        let valid = IndexSet(rows.filter { $0 >= 0 && $0 < Int(pane.rowCount) })
        guard !valid.isEmpty else { return }
        for row in valid {
            if let view = table.rowView(atRow: row, makeIfNecessary: false) as? FileRowView {
                configure(view, row: row)
            }
        }
        table.reloadData(forRowIndexes: valid, columnIndexes: IndexSet(integersIn: 0..<table.numberOfColumns))
    }

    func focusChanged() {
        guard let pane else { return }
        redraw(rows: [Int(pane.cursor)])
        store.listFocusChanged(side, focused: focused)
    }

    func observeWidth(of scroll: NSScrollView) {
        let center = NotificationCenter.default
        keyObservers.append(center.addObserver(forName: NSView.frameDidChangeNotification, object: scroll, queue: .main) {
            [weak self] _ in MainActor.assumeIsolated { self?.updateColumns() }
        })
        for name in [NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification] {
            keyObservers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] note in
                let window = (note.object as AnyObject?).map(ObjectIdentifier.init)
                MainActor.assumeIsolated {
                    guard let self, let window, window == self.table?.window.map(ObjectIdentifier.init) else { return }
                    self.focusChanged()
                }
            })
        }
    }

    /// Drops columns in narrow panes: Permissions, then Modified, then Ext.
    func updateColumns() {
        guard let table, let width = table.enclosingScrollView?.frame.width else { return }
        let n = width < 360 ? 3 : width < 470 ? 2 : width < 590 ? 1 : 0
        guard n != narrow else { return }
        narrow = n
        table.tableColumn(withIdentifier: FileColumn.mode.identifier)?.isHidden = n >= 1
        table.tableColumn(withIdentifier: FileColumn.date.identifier)?.isHidden = n >= 2
        table.tableColumn(withIdentifier: FileColumn.ext.identifier)?.isHidden = n >= 3
        table.sizeLastColumnToFit()
    }

    /// Widths that fit what the fixed columns hold at this font size.
    private func fitColumns(fontSize: CGFloat) {
        guard let table else { return }
        func width(_ sample: String, _ font: NSFont) -> CGFloat {
            ceil((sample as NSString).size(withAttributes: [.font: font]).width) + 10
        }
        let digits = NSFont.monospacedDigitSystemFont(ofSize: fontSize, weight: .regular)
        let mono = NSFont.monospacedSystemFont(ofSize: max(fontSize - 2, 9), weight: .regular)
        let widths: [FileColumn: CGFloat] = [
            .ext: width("ext.", .systemFont(ofSize: fontSize)) + 14,
            .size: width("999.9 MB", digits),
            .date: width("10/15/2026 12:32 PM", digits),
            .mode: width("drwxrwxrwx", mono),
        ]
        for (column, w) in widths {
            table.tableColumn(withIdentifier: column.identifier)?.width = max(w, column.width)
        }
    }

    private func updateSortIndicator() {
        guard let table, let pane else { return }
        for column in table.tableColumns {
            let key = FileColumn(column.identifier)?.sortKey
            let image = key == pane.sortKey
                ? NSImage(named: pane.sortDescending ? "NSDescendingSortIndicator" : "NSAscendingSortIndicator")
                : nil
            table.setIndicatorImage(image, in: column)
            if key == pane.sortKey { table.highlightedTableColumn = column }
        }
    }

    // ----- rows ---------------------------------------------------------------

    func row(_ ix: Int) -> Row? {
        if let row = cache[ix] { return row }
        guard let pane, ix >= 0, ix < Int(pane.rowCount) else { return nil }
        if cache.count > 4096 { cache.removeAll(keepingCapacity: true) }
        let start = (ix / Self.page) * Self.page
        for row in store.core.rows(side: side, start: UInt32(start), count: UInt32(Self.page)) {
            cache[Int(row.index)] = row
        }
        return cache[ix]
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        Int(pane?.rowCount ?? 0)
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        let view = (tableView.makeView(withIdentifier: FileRowView.identifier, owner: nil) as? FileRowView) ?? FileRowView()
        view.identifier = FileRowView.identifier
        configure(view, row: row)
        return view
    }

    private func configure(_ view: FileRowView, row: Int) {
        let cursor = pane.map { Int($0.cursor) == row } ?? false
        view.cursor = cursor
        view.focused = focused
        view.marked = self.row(row)?.marked ?? false
        view.colors = colors ?? .system
        view.needsDisplay = true
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row ix: Int) -> NSView? {
        guard let column = FileColumn(tableColumn?.identifier), let row = row(ix), let pane, let general else { return nil }
        let cursor = Int(pane.cursor) == ix
        let style = RowStyle(row: row, cursor: cursor, focused: cursor && focused, fontSize: CGFloat(general.fontSize),
                             colors: colors ?? .system)
        if column == .name {
            let cell = (tableView.makeView(withIdentifier: NameCell.identifier, owner: nil) as? NameCell) ?? NameCell()
            cell.show(row, style: style, icons: general.showIcons)
            return cell
        }
        let cell = (tableView.makeView(withIdentifier: TextCell.identifier, owner: nil) as? TextCell) ?? TextCell()
        cell.show(column, row, style: style)
        return cell
    }

    func tableView(_ tableView: NSTableView, didClick tableColumn: NSTableColumn) {
        guard let key = FileColumn(tableColumn.identifier)?.sortKey else { return }
        store.pane(side, .sortBy(key: key))
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        // Only the cursor is mirrored as the selection, programmatically.
        true
    }

    // ----- drops --------------------------------------------------------------

    func tableView(
        _ tableView: NSTableView, validateDrop info: NSDraggingInfo, proposedRow row: Int,
        proposedDropOperation dropOperation: NSTableView.DropOperation
    ) -> NSDragOperation {
        let board = info.draggingPasteboard
        if let data = board.data(forType: .translocatorFiles),
           let payload = try? JSONDecoder().decode(DragPayload.self, from: data)
        {
            guard payload.dragged.side != side else { return [] }
            tableView.setDropRow(-1, dropOperation: .on)
            return Self.moving ? .move : .copy
        }
        guard board.canReadObject(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) else { return [] }
        tableView.setDropRow(-1, dropOperation: .on)
        return .copy
    }

    func tableView(
        _ tableView: NSTableView, acceptDrop info: NSDraggingInfo, row: Int,
        dropOperation: NSTableView.DropOperation
    ) -> Bool {
        let board = info.draggingPasteboard
        if let data = board.data(forType: .translocatorFiles),
           let payload = try? JSONDecoder().decode(DragPayload.self, from: data)
        {
            guard payload.dragged.side != side else { return false }
            store.pane(side, .drop(drag: payload.dragged, mode: Self.moving ? .move : .copy))
            return true
        }
        let urls = board.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        guard !urls.isEmpty else { return false }
        store.send(.dropExternal(side: side, paths: urls.map(\.path)))
        return true
    }

    /// ⇧ or ⌘ held at the drop moves instead of copying.
    private static var moving: Bool {
        let flags = NSEvent.modifierFlags
        return flags.contains(.shift) || flags.contains(.command)
    }
}

/// How a row is drawn.
struct RowStyle {
    let row: Row
    let cursor: Bool
    /// The cursor, in the list with the keyboard: the accent band.
    let focused: Bool
    let fontSize: CGFloat
    /// The content layer's scheme (`Theme`).
    let colors: ContentColors

    var text: NSColor {
        if focused && (colors.isSystem || !row.marked) { return colors.cursorText }
        if row.marked { return colors.marked }
        return colors.text
    }

    var muted: NSColor {
        focused ? colors.cursorSecondary : colors.secondary
    }

    var icon: NSColor {
        if focused && colors.isSystem { return colors.cursorText }
        if row.isDir { return colors.folder }
        if row.executable { return colors.executable }
        return colors.secondary
    }

    var font: NSFont {
        row.marked ? .systemFont(ofSize: fontSize, weight: .semibold) : .systemFont(ofSize: fontSize)
    }

    var alpha: CGFloat { row.hidden ? 0.6 : 1 }
}

final class FileRowView: NSTableRowView {
    static let identifier = NSUserInterfaceItemIdentifier("FileRow")
    var cursor = false
    var focused = false
    var marked = false
    var colors = ContentColors.system

    override func drawBackground(in dirtyRect: NSRect) {
        let color: NSColor? =
            if cursor && focused { colors.cursor }
            else if cursor { colors.cursorInactive }
            else if marked { colors.markedBackground }
            else { nil }
        guard let color else { return }
        color.setFill()
        bounds.fill()
    }

    override func drawSelection(in dirtyRect: NSRect) {}
}

@MainActor
private func label() -> NSTextField {
    let field = NSTextField(labelWithString: "")
    field.lineBreakMode = .byTruncatingTail
    field.translatesAutoresizingMaskIntoConstraints = false
    field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    return field
}

final class NameCell: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("NameCell")
    private let icon = NSImageView()
    private let name = label()
    private let link = label()
    private var iconWidth: NSLayoutConstraint!

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.imageScaling = .scaleProportionallyDown
        link.setContentHuggingPriority(.required, for: .horizontal)
        for view in [icon, name, link] { addSubview(view) }
        iconWidth = icon.widthAnchor.constraint(equalToConstant: 16)
        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconWidth,
            name.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 5),
            name.centerYAnchor.constraint(equalTo: centerYAnchor),
            link.leadingAnchor.constraint(equalTo: name.trailingAnchor, constant: 6),
            link.centerYAnchor.constraint(equalTo: centerYAnchor),
            link.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -2),
            link.widthAnchor.constraint(lessThanOrEqualToConstant: 220),
        ])
        textField = name
        imageView = icon
    }

    required init?(coder: NSCoder) { fatalError() }

    func show(_ row: Row, style: RowStyle, icons: Bool) {
        name.stringValue = row.name
        name.font = style.font
        name.textColor = style.text
        link.stringValue = row.link ?? ""
        link.isHidden = row.link == nil
        link.font = .systemFont(ofSize: max(style.fontSize - 2, 9))
        link.textColor = style.muted
        icon.isHidden = !icons
        iconWidth.constant = icons ? 16 : 0
        if icons {
            let config = NSImage.SymbolConfiguration(pointSize: style.fontSize, weight: .regular)
            icon.image = symbolImage(row.icon.symbol)?.withSymbolConfiguration(config)
            icon.contentTintColor = style.icon
        }
        alphaValue = style.alpha
        var description = row.isParent ? "Enclosing folder" : row.name
        if row.isDir && !row.isParent { description += ", folder" }
        if row.marked { description += ", marked" }
        setAccessibilityLabel(description)
    }
}

final class TextCell: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("TextCell")
    private let field = label()

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        addSubview(field)
        NSLayoutConstraint.activate([
            field.leadingAnchor.constraint(equalTo: leadingAnchor),
            field.trailingAnchor.constraint(equalTo: trailingAnchor),
            field.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        textField = field
    }

    required init?(coder: NSCoder) { fatalError() }

    func show(_ column: FileColumn, _ row: Row, style: RowStyle) {
        let small = max(style.fontSize - 2, 9)
        switch column {
        case .ext:
            field.stringValue = row.ext
            field.font = .systemFont(ofSize: style.fontSize)
            field.textColor = style.muted
        case .size:
            field.stringValue = row.size
            let dir = row.isDir || row.isParent
            field.font = .monospacedDigitSystemFont(ofSize: dir ? small : style.fontSize, weight: .regular)
            field.textColor = dir ? style.muted : style.text
        case .date:
            field.stringValue = row.date
            field.font = .monospacedDigitSystemFont(ofSize: style.fontSize, weight: .regular)
            field.textColor = style.muted
        case .mode:
            field.stringValue = row.mode
            field.font = .monospacedSystemFont(ofSize: small, weight: .regular)
            field.textColor = style.muted
        case .name:
            break
        }
        field.alignment = column.alignment
        alphaValue = style.alpha
    }
}
