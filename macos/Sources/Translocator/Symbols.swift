import AppKit

/// SF Symbols for what the core describes.
extension IconKind {
    var symbol: String {
        switch self {
        case .parent: "arrow.turn.left.up"
        case .folder: "folder.fill"
        case .folderSymlink: "folder.badge.questionmark"
        case .fileSymlink: "link"
        case .special: "gearshape"
        case .archive: "doc.zipper"
        case .image: "photo"
        case .audio: "music.note"
        case .video: "film"
        case .code: "chevron.left.forwardslash.chevron.right"
        case .config: "doc.badge.gearshape"
        case .script: "terminal"
        case .text: "doc.text"
        case .key: "key"
        case .file: "doc"
        }
    }
}

extension LocationKind {
    var symbol: String {
        switch self {
        case .local: "internaldrive"
        case .remote: "server.rack"
        case .archive: "archivebox"
        }
    }
}

extension FolderIcon {
    var symbol: String {
        switch self {
        case .home: "house"
        case .desktop: "menubar.dock.rectangle"
        case .documents: "doc.text"
        case .downloads: "arrow.down.circle"
        case .volume: "externaldrive"
        case .folder: "folder"
        }
    }
}

extension ToastKind {
    var symbol: String {
        switch self {
        case .info: "info.circle.fill"
        case .success: "checkmark.circle.fill"
        case .error: "exclamationmark.triangle.fill"
        }
    }
}

extension Side {
    var other: Side { self == .left ? .right : .left }
    var name: String { self == .left ? "Left" : "Right" }
}

/// An `NSImage` of a symbol, for AppKit views and menus.
func symbolImage(_ name: String, label: String? = nil) -> NSImage? {
    NSImage(systemSymbolName: name, accessibilityDescription: label)
}
