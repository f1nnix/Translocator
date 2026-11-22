import AppKit
import SwiftUI
import TLCore

/// The shared key table, indexed for the menus and the file list's keys.
///
/// Keys are matched in the core's canonical spelling (`shift-cmd-g`:
/// ctrl, alt, shift, cmd, then the key), which `KeyMap.spellings(of:)`
/// builds from a key event.
struct KeyMap {
    let commands: [KeyCommand]
    private let byCommand: [Command: KeyCommand]
    private let byId: [String: KeyCommand]
    /// Single keystrokes by canonical spelling, per context.
    private let byKey: [KeyContext: [String: KeyTarget]]

    init(_ commands: [KeyCommand]) {
        self.commands = commands
        var byCommand: [Command: KeyCommand] = [:]
        var byId: [String: KeyCommand] = [:]
        var byKey: [KeyContext: [String: KeyTarget]] = [:]
        for command in commands {
            byId[command.id] = command
            if case .command(let cmd) = command.target, byCommand[cmd] == nil {
                byCommand[cmd] = command
            }
            for binding in command.bindings where binding.strokes.count == 1 {
                byKey[command.context, default: [:]][binding.canonical] = command.target
            }
        }
        self.byCommand = byCommand
        self.byId = byId
        self.byKey = byKey
    }

    func command(_ id: String) -> KeyCommand? { byId[id] }

    /// "⇧⌘G": the first binding of a command, as a label.
    func label(_ id: String) -> String? { byId[id]?.bindings.first?.pretty }

    func label(for command: Command) -> String? { byCommand[command]?.bindings.first?.pretty }

    func context(of command: Command) -> KeyContext { byCommand[command]?.context ?? .workspace }

    /// The shortcut a menu item shows and answers to: the command's first
    /// binding, when it is one a menu can carry without taking keys from
    /// text fields (it has ⌘ or ⌃, or is a function key) and is not one of
    /// the Edit menu's own.
    func menuShortcut(for command: Command) -> KeyboardShortcut? {
        guard let binding = byCommand[command]?.bindings.first, binding.strokes.count == 1 else { return nil }
        if KeyMap.editKeys.contains(binding.canonical) { return nil }
        return KeyMap.shortcut(binding.strokes[0], menu: true)
    }

    /// What a key pressed in a context does, trying the contexts in order.
    func target(for event: NSEvent, in contexts: [KeyContext]) -> KeyTarget? {
        let spellings = KeyMap.spellings(of: event)
        for context in contexts {
            guard let map = byKey[context] else { continue }
            for spelling in spellings {
                if let target = map[spelling] { return target }
            }
        }
        return nil
    }

    func target(spelling: String, in context: KeyContext) -> KeyTarget? {
        byKey[context]?[spelling]
    }

    /// The Edit menu keeps these for text (and routes them to the file list
    /// when it has the keyboard).
    static let editKeys: Set<String> = ["cmd-a", "cmd-c", "cmd-x", "cmd-v", "cmd-z", "shift-cmd-z"]

    // ----- key events -----------------------------------------------------

    private static let named: [UInt16: String] = [
        126: "up", 125: "down", 123: "left", 124: "right",
        116: "pageup", 121: "pagedown", 115: "home", 119: "end",
        36: "enter", 76: "enter", 48: "tab", 53: "escape",
        51: "backspace", 117: "delete", 49: "space", 114: "insert",
        122: "f1", 120: "f2", 99: "f3", 118: "f4", 96: "f5", 97: "f6",
        98: "f7", 100: "f8", 101: "f9", 109: "f10", 103: "f11", 111: "f12",
        105: "f13", 107: "f14", 113: "f15", 106: "f16", 64: "f17", 79: "f18",
        80: "f19", 90: "f20",
    ]

    /// The spellings a key event may be bound under: the unshifted key with
    /// its modifiers (`shift-cmd-.`), and for a shifted character also the
    /// character it typed (`cmd->`).
    static func spellings(of event: NSEvent) -> [String] {
        let flags = event.modifierFlags
        func spell(_ key: String, shift: Bool) -> String {
            var out = ""
            if flags.contains(.control) { out += "ctrl-" }
            if flags.contains(.option) { out += "alt-" }
            if shift { out += "shift-" }
            if flags.contains(.command) { out += "cmd-" }
            return out + key
        }
        let shift = flags.contains(.shift)
        if let name = named[event.keyCode] {
            return [spell(name, shift: shift)]
        }
        guard let base = event.characters(byApplyingModifiers: [])?.lowercased(), !base.isEmpty else { return [] }
        var out = [spell(base, shift: shift)]
        if shift, let typed = event.charactersIgnoringModifiers, !typed.isEmpty, typed.lowercased() != base {
            out.append(spell(typed, shift: false))
        }
        return out
    }

    // ----- menu shortcuts ---------------------------------------------------

    private static let equivalents: [String: KeyEquivalent] = [
        "up": .upArrow, "down": .downArrow, "left": .leftArrow, "right": .rightArrow,
        "enter": .return, "tab": .tab, "space": .space, "backspace": .delete,
        "delete": .deleteForward, "escape": .escape, "pageup": .pageUp, "pagedown": .pageDown,
        "home": .home, "end": .end,
    ]

    static func functionKey(_ key: String) -> Int? {
        guard key.hasPrefix("f"), let n = Int(key.dropFirst()), (1...20).contains(n) else { return nil }
        return n
    }

    /// A keystroke as a SwiftUI shortcut. With `menu`, only those a menu
    /// can own: ⌘ or ⌃ held, or a function key.
    static func shortcut(_ stroke: KeyStroke, menu: Bool) -> KeyboardShortcut? {
        let fkey = functionKey(stroke.key)
        if menu && !(stroke.platform || stroke.control || fkey != nil) { return nil }
        let key: KeyEquivalent
        if let fkey {
            guard let scalar = UnicodeScalar(NSF1FunctionKey + fkey - 1) else { return nil }
            key = KeyEquivalent(Character(scalar))
        } else if stroke.key == "insert" {
            guard let scalar = UnicodeScalar(NSInsertFunctionKey) else { return nil }
            key = KeyEquivalent(Character(scalar))
        } else if let named = equivalents[stroke.key] {
            key = named
        } else if stroke.key.count == 1, let ch = stroke.key.first {
            key = KeyEquivalent(ch)
        } else {
            return nil
        }
        var modifiers: EventModifiers = []
        if stroke.control { modifiers.insert(.control) }
        if stroke.alt { modifiers.insert(.option) }
        if stroke.shift { modifiers.insert(.shift) }
        if stroke.platform { modifiers.insert(.command) }
        return KeyboardShortcut(key, modifiers: modifiers)
    }

    /// The same for an AppKit menu item (the locations and context menus
    /// show them; they fire only while their menu is open).
    static func apply(_ binding: KeyBinding?, to item: NSMenuItem) {
        guard let binding, binding.strokes.count == 1 else { return }
        let stroke = binding.strokes[0]
        let named: [String: Int] = [
            "up": NSUpArrowFunctionKey, "down": NSDownArrowFunctionKey, "left": NSLeftArrowFunctionKey,
            "right": NSRightArrowFunctionKey, "enter": 0x0D, "tab": 0x09, "space": 0x20, "backspace": 0x08,
            "delete": NSDeleteFunctionKey, "escape": 0x1B, "pageup": NSPageUpFunctionKey,
            "pagedown": NSPageDownFunctionKey, "home": NSHomeFunctionKey, "end": NSEndFunctionKey,
            "insert": NSInsertFunctionKey,
        ]
        let code: Int?
        if let fkey = functionKey(stroke.key) {
            code = NSF1FunctionKey + fkey - 1
        } else {
            code = named[stroke.key]
        }
        if let code, let scalar = UnicodeScalar(code) {
            item.keyEquivalent = String(Character(scalar))
        } else if stroke.key.count == 1 {
            item.keyEquivalent = stroke.key
        } else {
            return
        }
        var mask: NSEvent.ModifierFlags = []
        if stroke.control { mask.insert(.control) }
        if stroke.alt { mask.insert(.option) }
        if stroke.shift { mask.insert(.shift) }
        if stroke.platform { mask.insert(.command) }
        item.keyEquivalentModifierMask = mask
    }
}

extension KeyMap {
    func binding(for command: Command) -> KeyBinding? { byCommand[command]?.bindings.first }
}
