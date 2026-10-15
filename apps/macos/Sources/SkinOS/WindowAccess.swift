import AppKit
import SwiftUI

/// Keeps the notification activation path attached to the actual client window.
struct WindowAccess: NSViewRepresentable {
    let attached: (NSWindow?) -> Void
    func makeNSView(context: Context) -> WindowObserver {
        let view = WindowObserver()
        view.attached = attached
        return view
    }
    func updateNSView(_ nsView: WindowObserver, context: Context) { nsView.attached = attached }
    final class WindowObserver: NSView {
        var attached: ((NSWindow?) -> Void)?
        override func viewDidMoveToWindow() { super.viewDidMoveToWindow(); attached?(window) }
    }
}
