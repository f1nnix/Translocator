import AppKit
import SwiftUI

/// One file pane: the location line, tabs, saved folders, the file list
/// (with its column header) and the status line.
struct PaneView: View {
    let side: Side
    @Environment(AppStore.self) private var store

    var body: some View {
        let pane = store.pane(side)
        let focused = store.focusedPane == side
        VStack(spacing: 0) {
            PaneHeader(side: side, pane: pane, focused: focused)
            Divider()
            if pane.tabTitles.count > 1 {
                TabStrip(side: side, pane: pane)
                Divider()
            }
            if !pane.places.isEmpty {
                PlacesRow(side: side, pane: pane)
                Divider()
            }
            PaneContent(side: side, pane: pane)
                .overlay { PaneOverlays(side: side, pane: pane) }
            Divider()
            HStack(spacing: 8) {
                Text(pane.status)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 8)
                if let free = pane.free {
                    Text(free)
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            .frame(height: 22)
            .accessibilityElement(children: .combine)
        }
        .background(Color(nsColor: .textBackgroundColor))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(side.name) pane, \(pane.display)")
    }
}

/// The list, the columns or the icons, as the pane is set to show.
struct PaneContent: View {
    let side: Side
    let pane: PaneSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        let general = store.snapshot.settings.general
        let active = store.snapshot.active == side
        switch pane.viewMode {
        case .list:
            FileTable(side: side, pane: pane, general: general, active: active)
        case .columns:
            ColumnBrowser(side: side, pane: pane, general: general, active: active)
        case .grid:
            FileGrid(side: side, pane: pane, general: general, active: active)
        }
    }
}

/// How the pane shows its folder: list, columns or icons.
struct ViewPicker: View {
    let side: Side
    let mode: ViewMode
    @Environment(AppStore.self) private var store

    var body: some View {
        Picker("View", selection: Binding(get: { mode }, set: { store.pane(side, .viewAs(mode: $0)) })) {
            ForEach(ViewMode.all, id: \.self) { mode in
                Label(mode.title, systemImage: mode.symbol)
                    .help(help(mode))
                    .tag(mode)
            }
        }
        .pickerStyle(.segmented)
        .labelStyle(.iconOnly)
        .labelsHidden()
        .controlSize(.small)
        .fixedSize()
        .accessibilityLabel("\(side.name) pane view")
    }

    private func help(_ mode: ViewMode) -> String {
        let key = store.keys.label(for: .viewAs(mode: mode)).map { " (\($0))" } ?? ""
        return "View \(mode.title)\(key)"
    }
}

/// The location menu, the path bar, the spinner and the sudo button.
struct PaneHeader: View {
    let side: Side
    let pane: PaneSnapshot
    let focused: Bool
    @Environment(AppStore.self) private var store

    var body: some View {
        HStack(spacing: 4) {
            LocationButton(side: side, label: pane.locationLabel, symbol: pane.kind.symbol)
                .fixedSize()
            PathBar(side: side, crumbs: pane.crumbs, elevated: pane.elevated)
                .frame(maxWidth: .infinity)
            if pane.loading {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel("Loading")
            }
            ViewPicker(side: side, mode: pane.viewMode)
            if pane.caps.elevation {
                Button {
                    store.command(.toggleSudo, on: side)
                } label: {
                    if pane.elevated {
                        Label("sudo", systemImage: "lock.open.fill")
                            .foregroundStyle(.red)
                    } else {
                        Label("sudo", systemImage: "lock")
                            .labelStyle(.iconOnly)
                    }
                }
                .buttonStyle(.borderless)
                .help(pane.elevated
                      ? "Running as root (sudo). Click to drop privileges."
                      : "Elevate this pane with sudo (\(store.keys.label("toggle-sudo") ?? "⇧⌘S"))")
                .accessibilityLabel(pane.elevated ? "Drop root privileges" : "Work as root with sudo")
            }
        }
        .padding(.horizontal, 6)
        .frame(height: 32)
        // The pane with the keyboard carries a tinted location line, so
        // which one it is does not depend on a cursor row being in view.
        .background(focused ? Color.accentColor.opacity(0.10) : Color.clear)
    }
}

/// The pane's tabs, shown when there are two or more.
struct TabStrip: View {
    let side: Side
    let pane: PaneSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 2) {
                ForEach(Array(pane.tabTitles.enumerated()), id: \.offset) { ix, title in
                    let selected = ix == Int(pane.activeTab)
                    HStack(spacing: 4) {
                        Button(title) { store.pane(side, .selectTab(index: UInt32(ix))) }
                            .buttonStyle(.plain)
                            .lineLimit(1)
                        Button {
                            store.pane(side, .closeTab(index: UInt32(ix)))
                        } label: {
                            Image(systemName: "xmark")
                                .font(.system(size: 8, weight: .bold))
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .accessibilityLabel("Close \(title)")
                    }
                    .font(.callout)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(selected ? Color.accentColor.opacity(0.18) : Color.clear,
                                in: RoundedRectangle(cornerRadius: 5))
                    .accessibilityAddTraits(selected ? .isSelected : [])
                }
            }
            .padding(.horizontal, 4)
        }
        .frame(height: 26)
    }
}

/// The folders saved for this pane's host, and the ⌘D toggle.
struct PlacesRow: View {
    let side: Side
    let pane: PaneSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: "bookmark")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 3) {
                    ForEach(Array(pane.places.enumerated()), id: \.element.id) { ix, place in
                        chip(ix, place)
                    }
                }
            }
            let saved = pane.placeSaved
            let key = store.keys.label("toggle-place").map { " (\($0))" } ?? ""
            Button {
                store.command(.togglePlace, on: side)
            } label: {
                Image(systemName: saved ? "bookmark.slash" : "bookmark.fill")
            }
            .buttonStyle(.borderless)
            .disabled(pane.kind == .archive)
            .help(saved ? "Take this folder off the row\(key)" : "Save this folder on the row\(key)")
            .accessibilityLabel(saved ? "Remove this folder from the saved folders" : "Save this folder")
        }
        .font(.caption)
        .padding(.horizontal, 6)
        .frame(height: 24)
    }

    private func chip(_ ix: Int, _ place: PlaceItem) -> some View {
        let last = pane.places.count - 1
        let key = ix < 9 ? store.keys.label("go-to-place-\(ix + 1)") : nil
        return Button(place.name) { store.pane(side, .navigate(path: place.path)) }
            .buttonStyle(.plain)
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .foregroundStyle(place.current ? .primary : .secondary)
            .background(place.current ? Color.accentColor.opacity(0.22) : Color.clear,
                        in: RoundedRectangle(cornerRadius: 4))
            .help(key.map { "\(place.display)  \($0)" } ?? place.display)
            .contextMenu {
                Button("Rename…", systemImage: "pencil") { store.pane(side, .renamePlace(id: place.id)) }
                Button("Move Left", systemImage: "chevron.left") {
                    store.pane(side, .movePlace(id: place.id, delta: -1))
                }
                .disabled(ix == 0)
                Button("Move Right", systemImage: "chevron.right") {
                    store.pane(side, .movePlace(id: place.id, delta: 1))
                }
                .disabled(ix == last)
                Divider()
                Button("Remove", systemImage: "bookmark.slash") { store.pane(side, .removePlace(id: place.id)) }
            }
            .accessibilityAddTraits(place.current ? .isSelected : [])
    }
}

/// What lies over the list: the filter chip, "nothing matches", a listing
/// error with Retry, and a closed connection with Reconnect.
struct PaneOverlays: View {
    let side: Side
    let pane: PaneSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        ZStack {
            if let error = pane.error, pane.rowCount <= 1 {
                VStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle")
                        .foregroundStyle(.orange)
                    Text(error)
                        .multilineTextAlignment(.center)
                        .foregroundStyle(.secondary)
                    Button("Retry") { store.pane(side, .retry) }
                }
                .font(.callout)
                .padding(.horizontal, 16)
                .frame(maxHeight: .infinity, alignment: .top)
                .padding(.top, 40)
            }
            if !pane.filter.isEmpty && pane.shown == 0 {
                VStack(spacing: 4) {
                    Image(systemName: "magnifyingglass")
                    Text("Nothing here matches “\(pane.filter)”")
                        .font(.callout)
                    Text("Esc shows everything again")
                        .font(.caption)
                }
                .foregroundStyle(.secondary)
                .frame(maxHeight: .infinity, alignment: .top)
                .padding(.top, 40)
            }
            if !pane.filter.isEmpty {
                HStack(spacing: 4) {
                    Image(systemName: "magnifyingglass")
                        .foregroundStyle(.secondary)
                    Text(pane.filter)
                    Text("\(pane.shown) of \(pane.unfiltered)")
                        .foregroundStyle(.secondary)
                }
                .font(.caption)
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
                .glassEffect(.regular, in: .capsule)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomLeading)
                .padding(8)
                .allowsHitTesting(false)
                .accessibilityElement(children: .combine)
                .accessibilityLabel("Filter \(pane.filter), \(pane.shown) of \(pane.unfiltered)")
            }
            if pane.disconnected {
                VStack(spacing: 10) {
                    Image(systemName: "powerplug")
                        .font(.title2)
                        .foregroundStyle(.orange)
                    Text("Connection closed")
                    Button("Reconnect") { store.pane(side, .reconnect) }
                        .buttonStyle(.borderedProminent)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color(nsColor: .textBackgroundColor).opacity(0.88))
            }
        }
    }
}
