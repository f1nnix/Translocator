import AppKit

/// The view of a pane that takes the keyboard: the list, the grid, or the
/// column of the pane's own folder in the column view.
@MainActor
protocol PaneFocusable: NSView {
    func takeFocus()
    func popUpLocations()
}

/// What every view of a pane does with a key: the core's key table first,
/// then the type-ahead filter. Answers whether the key was taken.
@MainActor
enum PaneKeys {
    static func keyDown(_ event: NSEvent, store: AppStore, side: Side, pane: PaneSnapshot, page: UInt32) -> Bool {
        if event.keyCode == 53 {
            // Escape clears the filter; with none it is left alone.
            if !pane.filter.isEmpty { store.command(.clearFilter, on: side) }
            return true
        }
        if let target = store.keys.target(for: event, in: [.pane, .workspace, .global]),
           case .command(let command) = target
        {
            switch command {
            case .pageUp: store.command(.pageUp(rows: page), on: side)
            case .pageDown: store.command(.pageDown(rows: page), on: side)
            default: store.command(command, on: side)
            }
            return true
        }
        let flags = event.modifierFlags.intersection([.command, .control, .option])
        if flags.isEmpty, let text = event.characters, !text.isEmpty,
           !text.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) || $0.value >= 0xF700 })
        {
            // The type-ahead filter; with no filter yet, + and - ask for a mask.
            store.command(.type(text: text), on: side)
            return true
        }
        return false
    }
}

/// Dragging rows out of a pane: to the other pane, or as files to Finder
/// and other applications.
@MainActor
enum PaneDrag {
    static func start(from view: NSView & NSDraggingSource, store: AppStore, side: Side, row: Int, event: NSEvent) {
        guard let items = store.core.drag(side: side, row: UInt32(row)) else { return }
        guard let payload = try? JSONEncoder().encode(DragPayload(items.dragged)) else { return }
        let image = image(items.label)
        let origin = view.convert(event.locationInWindow, from: nil)
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
        view.beginDraggingSession(with: dragItems, event: event, source: view)
    }

    /// Other applications copy; a drop on the other pane says itself.
    static func mask(_ context: NSDraggingContext) -> NSDragOperation {
        context == .outsideApplication ? .copy : [.copy, .move, .generic]
    }

    private static func image(_ label: String) -> NSImage {
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
}

/// Drops on a pane: rows from the other pane, or files from elsewhere.
@MainActor
enum PaneDrop {
    static let types: [NSPasteboard.PasteboardType] = [.translocatorFiles, .fileURL]

    /// What a drop would do, or `[]` when this pane does not take it.
    static func validate(_ info: NSDraggingInfo, side: Side) -> NSDragOperation {
        let board = info.draggingPasteboard
        if let payload = payload(board) {
            return payload.dragged.side == side ? [] : (moving ? .move : .copy)
        }
        guard board.canReadObject(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) else { return [] }
        return .copy
    }

    static func accept(_ info: NSDraggingInfo, side: Side, store: AppStore) -> Bool {
        let board = info.draggingPasteboard
        if let payload = payload(board) {
            guard payload.dragged.side != side else { return false }
            store.pane(side, .drop(drag: payload.dragged, mode: moving ? .move : .copy))
            return true
        }
        let urls = board.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        guard !urls.isEmpty else { return false }
        store.send(.dropExternal(side: side, paths: urls.map(\.path)))
        return true
    }

    private static func payload(_ board: NSPasteboard) -> DragPayload? {
        guard let data = board.data(forType: .translocatorFiles) else { return nil }
        return try? JSONDecoder().decode(DragPayload.self, from: data)
    }

    /// ⇧ or ⌘ held at the drop moves instead of copying.
    private static var moving: Bool {
        let flags = NSEvent.modifierFlags
        return flags.contains(.shift) || flags.contains(.command)
    }
}

/// Rows fetched by range and kept until their version moves.
@MainActor
struct RowCache {
    private var rows: [Int: Row] = [:]
    private static let page = 64

    mutating func clear() { rows.removeAll(keepingCapacity: true) }

    mutating func row(_ ix: Int, count: Int, fetch: (_ start: UInt32, _ count: UInt32) -> [Row]) -> Row? {
        if let row = rows[ix] { return row }
        guard ix >= 0, ix < count else { return nil }
        if rows.count > 4096 { rows.removeAll(keepingCapacity: true) }
        let start = (ix / Self.page) * Self.page
        for row in fetch(UInt32(start), UInt32(Self.page)) {
            rows[Int(row.index)] = row
        }
        return rows[ix]
    }
}

extension ViewMode {
    var symbol: String {
        switch self {
        case .list: "list.bullet"
        case .columns: "rectangle.split.3x1"
        case .grid: "square.grid.2x2"
        }
    }

    var title: String {
        switch self {
        case .list: "as List"
        case .columns: "as Columns"
        case .grid: "as Icons"
        }
    }

    static let all: [ViewMode] = [.list, .columns, .grid]
}
