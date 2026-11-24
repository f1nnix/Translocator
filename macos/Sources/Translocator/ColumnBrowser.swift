import AppKit
import SwiftUI

/// A pane in the column view, the way Finder draws one: the folders above
/// as columns to the left, the pane's own folder, and to its right the
/// folder under the cursor (or what the file under it is).
///
/// The pane's own column is the same table as the list, cut to one column
/// of names, so keys, marks, the filter, dragging and the context menu all
/// work as they do there. It stays the same view while the pane walks, so
/// it keeps the keyboard; only its place in the strip moves.
struct ColumnBrowser: NSViewRepresentable {
    let side: Side
    let pane: PaneSnapshot
    let general: GeneralSettings
    let active: Bool
    /// Read here so that a change of colour scheme updates the columns.
    var colors = Theme.shared.colors
    @Environment(AppStore.self) private var store

    func makeCoordinator() -> ColumnBrowserCoordinator {
        ColumnBrowserCoordinator(side: side, store: store)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let view = context.coordinator.makeView()
        context.coordinator.apply(pane: pane, general: general, active: active, colors: colors)
        return view
    }

    func updateNSView(_ view: NSScrollView, context: Context) {
        context.coordinator.apply(pane: pane, general: general, active: active, colors: colors)
    }
}

/// The strip's coordinates run top-down, like the columns' rows.
private final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}

@MainActor
final class ColumnBrowserCoordinator: NSObject {
    static let width: CGFloat = 220

    let side: Side
    unowned let store: AppStore
    private let own: FileTableCoordinator
    private var ownScroll: NSScrollView!
    private let outer = NSScrollView()
    private let strip = FlippedView()
    /// One per folder left of the pane's own, reused as the pane walks.
    private var lefts: [ColumnList] = []
    private var right: ColumnList!
    private let info = NSHostingView(rootView: FileInfoView(row: nil, colors: .system))
    private var dividers: [NSBox] = []
    private var pane: PaneSnapshot?
    private var general: GeneralSettings?
    private var colors = ContentColors.system
    private var observer: NSObjectProtocol?

    init(side: Side, store: AppStore) {
        self.side = side
        self.store = store
        own = FileTableCoordinator(side: side, store: store)
    }

    func makeView() -> NSScrollView {
        ownScroll = own.makeScroll(compact: true)
        right = ColumnList(side: side, store: store)
        outer.documentView = strip
        outer.hasHorizontalScroller = true
        outer.hasVerticalScroller = false
        outer.autohidesScrollers = true
        outer.borderType = .noBorder
        outer.drawsBackground = true
        outer.horizontalScrollElasticity = .allowed
        outer.verticalScrollElasticity = .none
        outer.postsFrameChangedNotifications = true
        strip.addSubview(ownScroll)
        strip.addSubview(right.view)
        strip.addSubview(info)
        strip.setAccessibilityLabel("\(side.name) pane columns")
        observer = NotificationCenter.default.addObserver(
            forName: NSView.frameDidChangeNotification, object: outer, queue: .main
        ) { [weak self] _ in MainActor.assumeIsolated { self?.layout(scroll: false) } }
        return outer
    }

    func apply(pane next: PaneSnapshot, general nextGeneral: GeneralSettings, active: Bool, colors nextColors: ContentColors) {
        let old = pane
        pane = next
        general = nextGeneral
        colors = nextColors
        outer.backgroundColor = nextColors.background
        own.apply(colors: nextColors)
        own.apply(pane: next, general: nextGeneral, active: active)
        while lefts.count < next.columns.count {
            let list = ColumnList(side: side, store: store)
            lefts.append(list)
            strip.addSubview(list.view)
        }
        for (ix, list) in lefts.enumerated() {
            if ix < next.columns.count {
                list.view.isHidden = false
                list.apply(next.columns[ix], index: ix, general: nextGeneral, colors: nextColors)
            } else {
                list.view.isHidden = true
            }
        }
        if let preview = next.preview {
            right.view.isHidden = false
            info.isHidden = true
            right.apply(preview, index: next.columns.count, general: nextGeneral, colors: nextColors)
        } else {
            right.view.isHidden = true
            let row = next.rowCount > 0 ? store.core.rows(side: side, start: next.cursor, count: 1).first : nil
            info.isHidden = row == nil
            info.rootView = FileInfoView(row: row, colors: nextColors)
        }
        let walked = old?.path != next.path || old?.columns.count != next.columns.count
            || (old?.preview == nil) != (next.preview == nil)
        layout(scroll: walked)
    }

    /// Places the columns side by side; with `scroll`, brings the pane's
    /// own column and the one to its right into view.
    private func layout(scroll: Bool) {
        guard let pane, ownScroll != nil else { return }
        let height = outer.contentSize.height
        let w = Self.width
        let shownLefts = pane.columns.count
        var x: CGFloat = 0
        var frames: [NSRect] = []
        for ix in 0..<shownLefts {
            lefts[ix].view.frame = NSRect(x: x, y: 0, width: w, height: height)
            frames.append(lefts[ix].view.frame)
            x += w + 1
        }
        let ownFrame = NSRect(x: x, y: 0, width: w, height: height)
        ownScroll.frame = ownFrame
        x += w + 1
        // The last column takes what is left of the pane, if anything is.
        let last = max(w, outer.contentSize.width - x)
        let rightFrame = NSRect(x: x, y: 0, width: last, height: height)
        right.view.frame = rightFrame
        info.frame = rightFrame
        let total = x + last
        strip.frame = NSRect(x: 0, y: 0, width: total, height: height)
        // A hairline after every column but the last.
        let count = shownLefts + 1
        while dividers.count < count {
            let box = NSBox()
            box.boxType = .custom
            box.borderWidth = 0
            strip.addSubview(box)
            dividers.append(box)
        }
        for (ix, box) in dividers.enumerated() {
            box.isHidden = ix >= count
            box.fillColor = colors.border
            box.frame = NSRect(x: CGFloat(ix + 1) * (w + 1) - 1, y: 0, width: 1, height: height)
        }
        if scroll {
            let target = NSRect(x: ownFrame.minX, y: 0, width: min(rightFrame.maxX, ownFrame.minX + 2 * w + 1) - ownFrame.minX,
                                height: 1)
            strip.scrollToVisible(target)
        }
    }
}

/// A column other than the pane's own: its folder's names, the one the
/// path goes on into drawn selected. A press moves the pane there.
@MainActor
final class ColumnList: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    let side: Side
    unowned let store: AppStore
    let view = NSView()
    private let scroll = NSScrollView()
    private let table = ColumnTableView()
    private let status = NSTextField(wrappingLabelWithString: "")
    private let spinner = NSProgressIndicator()
    private var column: ColumnSnapshot?
    private(set) var index = 0
    private var general: GeneralSettings?
    private var colors = ContentColors.system
    private var cache = RowCache()

    init(side: Side, store: AppStore) {
        self.side = side
        self.store = store
        super.init()
        table.list = self
        table.dataSource = self
        table.delegate = self
        table.headerView = nil
        table.style = .fullWidth
        table.selectionHighlightStyle = .none
        table.intercellSpacing = NSSize(width: 6, height: 0)
        table.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        let col = NSTableColumn(identifier: FileColumn.name.identifier)
        col.resizingMask = .autoresizingMask
        table.addTableColumn(col)
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.drawsBackground = true
        scroll.autoresizingMask = [.width, .height]
        view.addSubview(scroll)
        status.alignment = .center
        status.textColor = .secondaryLabelColor
        status.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        status.translatesAutoresizingMaskIntoConstraints = false
        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isDisplayedWhenStopped = false
        spinner.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(status)
        view.addSubview(spinner)
        NSLayoutConstraint.activate([
            status.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            status.topAnchor.constraint(equalTo: view.topAnchor, constant: 40),
            status.widthAnchor.constraint(lessThanOrEqualTo: view.widthAnchor, constant: -24),
            spinner.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            spinner.topAnchor.constraint(equalTo: view.topAnchor, constant: 40),
        ])
    }

    func apply(_ next: ColumnSnapshot, index: Int, general nextGeneral: GeneralSettings, colors nextColors: ContentColors) {
        let reload = column?.version != next.version || general != nextGeneral || colors != nextColors
        self.index = index
        if general?.rowHeight != nextGeneral.rowHeight { table.rowHeight = CGFloat(nextGeneral.rowHeight) }
        let selectionMoved = column?.selected != next.selected
        column = next
        general = nextGeneral
        colors = nextColors
        scroll.frame = view.bounds
        scroll.backgroundColor = nextColors.background
        table.backgroundColor = nextColors.background
        table.setAccessibilityLabel("Folder \(next.name)")
        status.stringValue = next.error ?? ""
        status.isHidden = next.error == nil
        if next.loading && next.rowCount == 0 { spinner.startAnimation(nil) } else { spinner.stopAnimation(nil) }
        if reload {
            cache.clear()
            table.reloadData()
        }
        if let selected = next.selected, reload || selectionMoved {
            table.scrollRowToVisible(Int(selected))
        }
    }

    func row(_ ix: Int) -> Row? {
        guard let column else { return nil }
        let (side, index) = (side, UInt32(index))
        return cache.row(ix, count: Int(column.rowCount)) { start, count in
            store.core.columnRows(side: side, column: index, start: start, count: count)
        }
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        Int(column?.rowCount ?? 0)
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        let view = (tableView.makeView(withIdentifier: FileRowView.identifier, owner: nil) as? FileRowView) ?? FileRowView()
        view.identifier = FileRowView.identifier
        view.cursor = self.row(row)?.cursor ?? false
        view.focused = false
        view.marked = false
        view.colors = colors
        return view
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row ix: Int) -> NSView? {
        guard let row = row(ix), let general else { return nil }
        let style = RowStyle(row: row, cursor: row.cursor, focused: false, fontSize: CGFloat(general.fontSize), colors: colors)
        let cell = (tableView.makeView(withIdentifier: NameCell.identifier, owner: nil) as? NameCell) ?? NameCell()
        cell.show(row, style: style, icons: general.showIcons, chevron: true)
        return cell
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool { false }

    /// A press on `row`: the pane moves to this column's folder, and the
    /// keyboard to the pane's own column.
    func pressed(row: Int) {
        guard let column, row >= 0, row < Int(column.rowCount) else { return }
        store.pane(side, .columnClick(column: UInt32(index), row: UInt32(row)))
        store.table(side)?.takeFocus()
    }
}

final class ColumnTableView: NSTableView {
    weak var list: ColumnList?

    override var acceptsFirstResponder: Bool { false }

    override func mouseDown(with event: NSEvent) {
        let row = row(at: convert(event.locationInWindow, from: nil))
        list?.pressed(row: row)
    }
}

/// The column right of a file: what it is.
struct FileInfoView: View {
    let row: Row?
    let colors: ContentColors

    var body: some View {
        if let row {
            VStack(spacing: 10) {
                Image(systemName: row.icon.symbol)
                    .font(.system(size: 56, weight: .light))
                    .foregroundStyle(Color(nsColor: row.isDir ? colors.folder : colors.secondary))
                    .padding(.top, 36)
                Text(row.name)
                    .font(.headline)
                    .multilineTextAlignment(.center)
                    .lineLimit(4)
                    .textSelection(.enabled)
                Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 4) {
                    if !row.ext.isEmpty { line("Kind", "\(row.ext.uppercased()) file") }
                    if !row.isDir { line("Size", row.size) }
                    if !row.date.isEmpty { line("Modified", row.date) }
                    if !row.mode.isEmpty { line("Permissions", row.mode) }
                    if let link = row.link { line("Link", link) }
                }
                .font(.caption)
                Spacer()
            }
            .padding(.horizontal, 14)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityElement(children: .combine)
        } else {
            Color.clear
        }
    }

    private func line(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label).foregroundStyle(.secondary).gridColumnAlignment(.trailing)
            Text(value).monospacedDigit()
        }
    }
}
