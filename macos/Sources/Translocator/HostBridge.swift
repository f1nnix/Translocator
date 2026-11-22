import AppKit
import Synchronization
import TLCore

/// What the core asks of the app, carried to the main thread.
enum HostEvent: Sendable {
    case toast(ToastKind, String)
    case clipboard(String)
    case open(String)
    case reveal(String)
    case openURL(String)
    case quit
}

/// The core's `Host`. Every call comes on a Rust thread: it hops to the
/// main actor and returns at once. Bursts of `changed()` collapse into one
/// snapshot refresh per run-loop turn.
final class HostBridge: TLCore.Host {
    private let pending = Mutex(false)
    @MainActor weak var store: AppStore?

    func changed() {
        let first = pending.withLock { pending in
            defer { pending = true }
            return !pending
        }
        guard first else { return }
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                self.pending.withLock { $0 = false }
                self.store?.refresh()
            }
        }
    }

    private func deliver(_ event: HostEvent) {
        DispatchQueue.main.async {
            MainActor.assumeIsolated { self.store?.perform(event) }
        }
    }

    func toast(kind: ToastKind, message: String) { deliver(.toast(kind, message)) }
    func writeClipboard(text: String) { deliver(.clipboard(text)) }
    func openPath(path: String) { deliver(.open(path)) }
    func revealPath(path: String) { deliver(.reveal(path)) }
    func openUrl(url: String) { deliver(.openURL(url)) }
    func quit() { deliver(.quit) }
}
