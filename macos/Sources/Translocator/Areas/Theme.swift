import AppKit
import Observation
import SwiftUI
import TLCore

/// The colour scheme of the content layer.
///
/// The window's chrome stays the system's own (the Liquid Glass toolbar,
/// sidebars, controls, materials). What the chosen scheme paints is the
/// content: the file lists, the console, and the viewer, editor and quick
/// look. "Default Light" and "Default Dark" are the system colours, so
/// `palette` is nil for them and every colour below is a system one.
///
/// Which theme applies follows the settings (`appearance.light`/`.dark`)
/// and the window's effective appearance, which the store sets from
/// `appearance.mode` (System, Light, Dark). Read `Theme.shared` from AppKit
/// code, or `@Environment(\.theme)` in SwiftUI; both are observable, so a
/// view that reads a colour redraws when the scheme changes.
@MainActor
@Observable
final class Theme {
    static let shared = Theme()

    /// The scheme in use, or nil for the system's colours.
    private(set) var palette: ThemePalette?
    /// The effective appearance is dark.
    private(set) var isDark: Bool
    /// The content colours, as AppKit colours.
    private(set) var colors: ContentColors

    @ObservationIgnored private var appearanceObserver: NSKeyValueObservation?
    @ObservationIgnored private var names: (light: String, dark: String) = ("", "")

    private init() {
        isDark = Theme.effectiveDark()
        colors = .system
        observeSettings()
        appearanceObserver = NSApp?.observe(\.effectiveAppearance, options: [.new]) { _, _ in
            DispatchQueue.main.async { MainActor.assumeIsolated { Theme.shared.update() } }
        }
        update()
    }

    /// The theme the settings name for the current appearance.
    var name: String { isDark ? names.dark : names.light }

    private static func effectiveDark() -> Bool {
        let appearance = NSApp?.effectiveAppearance ?? NSAppearance.currentDrawing()
        return appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
    }

    /// Follows `Snapshot.settings.appearance` without the store knowing.
    private func observeSettings() {
        let store = AppStore.shared
        let appearance = withObservationTracking {
            store.snapshot.settings.appearance
        } onChange: {
            DispatchQueue.main.async { MainActor.assumeIsolated { Theme.shared.observeSettings() } }
        }
        if appearance.light != names.light || appearance.dark != names.dark {
            names = (appearance.light, appearance.dark)
            update()
        }
    }

    private func update() {
        let dark = Theme.effectiveDark()
        if dark != isDark { isDark = dark }
        let next = TLCore.palette(name: dark ? names.dark : names.light)
        // A light scheme in a dark window (or the other way round) would
        // fight the chrome around it: the pickers only offer matching ones,
        // and a hand-edited file falls back to the system colours.
        let fitting = next.flatMap { $0.dark == dark ? $0 : nil }
        if fitting != palette {
            palette = fitting
            colors = fitting.map(ContentColors.init) ?? .system
        }
    }
}

/// The content layer's colours: a scheme's, or the system's.
struct ContentColors: Equatable {
    /// The canvas: list, console and editor background.
    var background: NSColor
    /// Raised surfaces over the canvas (column headers, status lines).
    var chrome: NSColor
    var text: NSColor
    /// Sizes, dates, permissions, comments.
    var secondary: NSColor
    var border: NSColor
    var accent: NSColor
    /// Every other row.
    var stripe: NSColor
    var hover: NSColor
    /// The cursor row in the list with the keyboard, and the text on it.
    var cursor: NSColor
    var cursorText: NSColor
    var cursorSecondary: NSColor
    /// The cursor row in the other list.
    var cursorInactive: NSColor
    /// A marked row's name, and what is behind it.
    var marked: NSColor
    var markedBackground: NSColor
    var folder: NSColor
    var executable: NSColor
    var red: NSColor
    var green: NSColor
    var yellow: NSColor
    var blue: NSColor
    var magenta: NSColor
    var cyan: NSColor
    /// The scheme is the system's own.
    var isSystem: Bool

    static let system = ContentColors(
        background: .textBackgroundColor,
        chrome: .windowBackgroundColor,
        text: .labelColor,
        secondary: .secondaryLabelColor,
        border: .separatorColor,
        accent: .controlAccentColor,
        stripe: .alternatingContentBackgroundColors.last ?? .textBackgroundColor,
        hover: .quaternarySystemFill,
        cursor: .selectedContentBackgroundColor,
        cursorText: .alternateSelectedControlTextColor,
        cursorSecondary: .alternateSelectedControlTextColor.withAlphaComponent(0.85),
        cursorInactive: .unemphasizedSelectedContentBackgroundColor,
        marked: .systemOrange,
        markedBackground: NSColor.systemOrange.withAlphaComponent(0.14),
        folder: .systemBlue,
        executable: .systemGreen,
        red: .systemRed,
        green: .systemGreen,
        yellow: .systemYellow,
        blue: .systemBlue,
        magenta: .systemPink,
        cyan: .systemTeal,
        isSystem: true
    )

    init(
        background: NSColor, chrome: NSColor, text: NSColor, secondary: NSColor, border: NSColor,
        accent: NSColor, stripe: NSColor, hover: NSColor, cursor: NSColor, cursorText: NSColor,
        cursorSecondary: NSColor, cursorInactive: NSColor, marked: NSColor, markedBackground: NSColor,
        folder: NSColor, executable: NSColor, red: NSColor, green: NSColor, yellow: NSColor,
        blue: NSColor, magenta: NSColor, cyan: NSColor, isSystem: Bool
    ) {
        self.background = background
        self.chrome = chrome
        self.text = text
        self.secondary = secondary
        self.border = border
        self.accent = accent
        self.stripe = stripe
        self.hover = hover
        self.cursor = cursor
        self.cursorText = cursorText
        self.cursorSecondary = cursorSecondary
        self.cursorInactive = cursorInactive
        self.marked = marked
        self.markedBackground = markedBackground
        self.folder = folder
        self.executable = executable
        self.red = red
        self.green = green
        self.yellow = yellow
        self.blue = blue
        self.magenta = magenta
        self.cyan = cyan
        self.isSystem = isSystem
    }

    /// A scheme's colours. The cursor band is translucent over the canvas,
    /// so the text on it stays the scheme's own, as in the GPUI shell.
    init(_ p: ThemePalette) {
        let c = NSColor.init(hex:)
        self.init(
            background: c(p.background), chrome: c(p.chrome), text: c(p.foreground), secondary: c(p.muted),
            border: c(p.border), accent: c(p.accent), stripe: c(p.stripe), hover: c(p.hover),
            cursor: c(p.selection), cursorText: c(p.foreground), cursorSecondary: c(p.muted),
            cursorInactive: c(p.selectionInactive), marked: c(p.marked), markedBackground: c(p.markedBackground),
            folder: c(p.folder), executable: c(p.executable), red: c(p.red), green: c(p.green),
            yellow: c(p.yellow), blue: c(p.blue), magenta: c(p.magenta), cyan: c(p.cyan), isSystem: false
        )
    }
}

extension NSColor {
    /// `#rrggbb` or `#rrggbbaa`, in sRGB; anything else is black.
    convenience init(hex: String) {
        let digits = hex.hasPrefix("#") ? hex.dropFirst() : Substring(hex)
        let value = UInt64(digits, radix: 16) ?? 0
        let hasAlpha = digits.count == 8
        let rgb = hasAlpha ? value >> 8 : value
        let byte = { (shift: UInt64) in CGFloat((rgb >> shift) & 0xFF) / 255 }
        let alpha = hasAlpha ? CGFloat(value & 0xFF) / 255 : 1
        self.init(srgbRed: byte(16), green: byte(8), blue: byte(0), alpha: alpha)
    }
}

private struct ThemeKey: EnvironmentKey {
    static var defaultValue: Theme { MainActor.assumeIsolated { Theme.shared } }
}

extension EnvironmentValues {
    /// The content layer's colour scheme (see `Theme`).
    var theme: Theme {
        get { self[ThemeKey.self] }
        set { self[ThemeKey.self] = newValue }
    }
}
