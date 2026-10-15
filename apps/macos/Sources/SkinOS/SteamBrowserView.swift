import AppKit
import SwiftUI
import WebKit

enum SteamWebPolicy {
    static let allowedHosts: Set<String> = ["store.steampowered.com", "steamcommunity.com", "checkout.steampowered.com", "login.steampowered.com", "help.steampowered.com"]
    static func allows(_ url: URL) -> Bool {
        url.scheme?.lowercased() == "https" && allowedHosts.contains(url.host?.lowercased() ?? "")
        && url.user == nil && url.password == nil && (url.port == nil || url.port == 443)
    }
}

@MainActor
final class SteamBrowserController: NSObject, ObservableObject, WKNavigationDelegate, WKUIDelegate {
    @Published var canGoBack = false
    @Published var canGoForward = false
    @Published var loading = false
    @Published var address = ""
    @Published var error: String?
    private(set) var webView: WKWebView?
    private var requestedURL: URL?
    private var appliedRevision = -1
    private var observations: [NSKeyValueObservation] = []

    func prepare(dataStore: WKWebsiteDataStore) {
        guard webView == nil else { return }
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = dataStore
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = self
        view.uiDelegate = self
        view.allowsBackForwardNavigationGestures = true
        view.setValue(false, forKey: "drawsBackground")
        observations = [view.observe(\.isLoading, options: [.new]) { [weak self] view, _ in
            Task { @MainActor in self?.loading = view.isLoading }
        }, view.observe(\.canGoBack, options: [.new]) { [weak self] view, _ in
            Task { @MainActor in self?.canGoBack = view.canGoBack }
        }, view.observe(\.canGoForward, options: [.new]) { [weak self] view, _ in
            Task { @MainActor in self?.canGoForward = view.canGoForward }
        }, view.observe(\.url, options: [.new]) { [weak self] view, _ in
            Task { @MainActor in
                // Do not show authentication query parameters or bearer cookies.
                self?.address = view.url.map { ($0.host ?? "") + $0.path } ?? ""
            }
        }]
        webView = view
    }

    func synchronize(cookies: [HTTPCookie], revision: Int, url: URL) async {
        guard let view = webView else { return }
        let changed = appliedRevision != revision
        if changed {
            for cookie in cookies {
                guard !Task.isCancelled else { return }
                await view.configuration.websiteDataStore.httpCookieStore.setCookie(cookie)
            }
            appliedRevision = revision
        }
        guard !Task.isCancelled else { return }
        if requestedURL != url { requestedURL = url; error = nil; view.load(URLRequest(url: url)) }
        else if changed { view.reload() }
    }

    func reload() { error = nil; webView?.reload() }
    func back() { webView?.goBack() }
    func forward() { webView?.goForward() }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let url = action.request.url else { decisionHandler(.cancel); return }
        if url.scheme == "about" { decisionHandler(.allow); return }
        let allowed = SteamWebPolicy.allows(url)
        if allowed {
            if action.targetFrame == nil { webView.load(action.request); decisionHandler(.cancel) }
            else { decisionHandler(.allow) }
        } else {
            decisionHandler(.cancel)
            if action.navigationType == .linkActivated, ["https", "http"].contains(url.scheme?.lowercased() ?? "") {
                NSWorkspace.shared.open(url)
            } else if url.scheme?.lowercased() == "steam" {
                error = "Эта ссылка открывает Steam. Игры можно запускать из библиотеки SkinOS."
            }
        }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) { show(error) }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) { show(error) }
    private func show(_ error: Error) {
        guard (error as NSError).code != NSURLErrorCancelled else { return }
        self.error = "Не удалось загрузить страницу. Проверьте подключение и повторите попытку."
    }
    func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping () -> Void) {
        let alert = NSAlert(); alert.messageText = "Steam"; alert.informativeText = message; alert.addButton(withTitle: "OK")
        if let window = webView.window { alert.beginSheetModal(for: window) { _ in completionHandler() } } else { completionHandler() }
    }
    func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (Bool) -> Void) {
        let alert = NSAlert(); alert.messageText = "Steam"; alert.informativeText = message
        alert.addButton(withTitle: "Подтвердить"); alert.addButton(withTitle: "Отмена")
        if let window = webView.window { alert.beginSheetModal(for: window) { completionHandler($0 == .alertFirstButtonReturn) } } else { completionHandler(false) }
    }
}

struct SteamWebSurface: NSViewRepresentable {
    let controller: SteamBrowserController
    func makeNSView(context: Context) -> WKWebView { controller.webView! }
    func updateNSView(_ view: WKWebView, context: Context) {}
    static func dismantleNSView(_ nsView: WKWebView, coordinator: ()) { nsView.stopLoading() }
}

struct SteamBrowserView: View {
    @ObservedObject var model: AppModel
    let target: String
    @StateObject private var browser = SteamBrowserController()
    @State private var search = ""
    @State private var prepared = false
    private var url: URL { target == "store" ? model.storeURL : model.profileURL }
    private var sessionReady: Bool { model.webSessionTargets.contains(target) }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 15) {
                Button(action: browser.back) { Image(systemName: "chevron.left") }.disabled(!browser.canGoBack).help("Назад")
                Button(action: browser.forward) { Image(systemName: "chevron.right") }.disabled(!browser.canGoForward).help("Вперёд")
                Button(action: browser.reload) { Image(systemName: "arrow.clockwise") }.disabled(!sessionReady).help("Обновить страницу")
                HStack(spacing: 7) {
                    Image(systemName: "lock.fill").font(.system(size: 9)).foregroundStyle(Theme.accent)
                    Text(browser.address.isEmpty ? (url.host ?? "Steam") : browser.address)
                        .font(.system(size: 10, design: .monospaced)).lineLimit(1).truncationMode(.middle)
                }.foregroundStyle(Theme.muted)
                Spacer(minLength: 10)
                if target == "store" {
                    HStack(spacing: 8) {
                        Image(systemName: "magnifyingglass").foregroundStyle(Theme.muted)
                        TextField("Поиск в магазине", text: $search).textFieldStyle(.plain).onSubmit(searchStore)
                    }.font(.system(size: 11)).padding(9).frame(width: 210).background(Theme.elevated, in: RoundedRectangle(cornerRadius: 7))
                } else {
                    Button("Мой профиль") { model.showProfile() }
                }
                if browser.loading { ProgressView().controlSize(.small) }
            }.buttonStyle(.plain).padding(.horizontal, 24).frame(height: 59).background(Theme.surface)
            if let error = browser.error ?? model.webSessionError {
                HStack {
                    Text(error).font(.system(size: 11)).foregroundStyle(.orange)
                    Spacer()
                    Button("Повторить") { browser.error = nil; model.loadWebSession(target: target, force: true) }
                }.padding(14).background(Color.orange.opacity(0.06))
            }
            if model.account == nil {
                WelcomeView(model: model)
            } else if sessionReady, prepared {
                SteamWebSurface(controller: browser)
            } else {
                VStack(spacing: 16) {
                    if model.webSessionLoading { ProgressView() }
                    Image(systemName: target == "store" ? "bag" : "person.crop.rectangle").font(.system(size: 32)).foregroundStyle(Theme.muted)
                    Text(model.webSessionLoading ? "Подключаем \(target == "store" ? "магазин" : "профиль") Steam…" : "Веб-сессия Steam")
                        .font(.system(size: 17, weight: .semibold))
                    Text("\(model.account?.displayName ?? "") · защищённое подключение")
                        .font(.system(size: 11)).foregroundStyle(Theme.muted)
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .onAppear { browser.prepare(dataStore: model.websiteDataStore); prepared = true; model.loadWebSession(target: target) }
        .task(id: "\(model.webSessionRevision)-\(url.absoluteString)") {
            if sessionReady { await browser.synchronize(cookies: model.webCookies, revision: model.webSessionRevision, url: url) }
        }
        .onChange(of: model.webSessionTargets) { _, targets in
            if targets.contains(target) { Task { await browser.synchronize(cookies: model.webCookies, revision: model.webSessionRevision, url: url) } }
        }
    }
    private func searchStore() {
        var components = URLComponents(string: "https://store.steampowered.com/search/")!
        components.queryItems = [URLQueryItem(name: "term", value: search.trimmingCharacters(in: .whitespacesAndNewlines))]
        if let url = components.url { model.storeURL = url }
    }
}
