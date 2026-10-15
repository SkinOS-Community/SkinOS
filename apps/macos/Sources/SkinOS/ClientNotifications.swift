import AppKit
import CryptoKit
import Foundation
import UserNotifications

struct NotificationPreferences: Codable, Equatable {
    var messages = true
    var steam = true
    var downloads = true
    var gameErrors = true
    var sound = true
    var previews = true
    var anyEnabled: Bool { messages || steam || downloads || gameErrors }
}

enum NotificationKind: String, Codable {
    case message, steam, download, game, test
}

enum NotificationTarget: Equatable {
    case chat(String), steamInbox, download(Int), game(Int), settings
    var fields: [String: String] {
        switch self {
        case .chat(let id): return ["destination": "chat", "target": id]
        case .steamInbox: return ["destination": "steam"]
        case .download(let id): return ["destination": "download", "target": String(id)]
        case .game(let id): return ["destination": "game", "target": String(id)]
        case .settings: return ["destination": "settings"]
        }
    }
    init?(fields: [AnyHashable: Any]) {
        switch fields["destination"] as? String {
        case "chat":
            guard let id = fields["target"] as? String, !id.isEmpty, id.allSatisfy(\.isNumber) else { return nil }
            self = .chat(id)
        case "steam": self = .steamInbox
        case "download", "game":
            guard let text = fields["target"] as? String, let id = Int(text), id > 0 else { return nil }
            self = fields["destination"] as? String == "download" ? .download(id) : .game(id)
        case "settings": self = .settings
        default: return nil
        }
    }
}

struct ClientNotice: Identifiable, Equatable {
    let id: String
    let owner: String
    let kind: NotificationKind
    let title: String
    let body: String
    let target: NotificationTarget
    let timestamp: Date
    var read = false
    var steamURL: URL? = nil

    var privateBody: String {
        switch kind {
        case .message: return "Новое сообщение Steam"
        case .steam: return "Новое уведомление Steam"
        case .download: return "Изменилось состояние загрузки"
        case .game: return "Сообщение о запуске игры"
        case .test: return body
        }
    }
    var fingerprint: String {
        SHA256.hash(data: Data((owner + ":" + kind.rawValue + ":" + id).utf8)).map { String(format: "%02x", $0) }.joined()
    }
}

enum NotificationPermission: String {
    case unknown, allowed, denied, unavailable
    var label: String {
        switch self {
        case .unknown: return "Нужно разрешение macOS"
        case .allowed: return "Уведомления разрешены"
        case .denied: return "Уведомления отключены в macOS"
        case .unavailable: return "Недоступны в этой сборке"
        }
    }
}

@MainActor
final class ClientNotifications: NSObject, ObservableObject, UNUserNotificationCenterDelegate {
    @Published var preferences: NotificationPreferences {
        didSet { if let data = try? JSONEncoder().encode(preferences) { defaults?.set(data, forKey: "notifications.preferences.v1") } }
    }
    @Published private(set) var permission: NotificationPermission = .unknown
    @Published private(set) var requestingPermission = false
    @Published private(set) var systemAlertsEnabled = true
    @Published private(set) var systemSoundEnabled = true
    @Published var statusMessage: String?
    static let shared = ClientNotifications()
    var onOpen: ((String, NotificationTarget) -> Void)? {
        didSet {
            if let pendingOpen, let onOpen { self.pendingOpen = nil; onOpen(pendingOpen.owner, pendingOpen.target) }
        }
    }
    private var pendingOpen: (owner: String, target: NotificationTarget)?
    var canPresent: ((String, NotificationTarget) -> Bool)?
    private(set) var activeAccount: String?
    private var session = UUID().uuidString
    private var center: UNUserNotificationCenter?
    private let defaults: UserDefaults?
    private let testDelivery: ((ClientNotice, Bool) -> Void)?
    private var seen: [String: [String]] = [:]
    private static let prefix = "app.skinos.notice."

    init(defaults: UserDefaults? = .standard, testDelivery: ((ClientNotice, Bool) -> Void)? = nil) {
        self.defaults = defaults
        self.testDelivery = testDelivery
        preferences = defaults?.data(forKey: "notifications.preferences.v1").flatMap { try? JSONDecoder().decode(NotificationPreferences.self, from: $0) } ?? NotificationPreferences()
        super.init()
        if testDelivery != nil { permission = .allowed }
    }

    func start() {
        guard center == nil, testDelivery == nil else { return }
        guard Bundle.main.bundleURL.pathExtension == "app" else { permission = .unavailable; return }
        center = UNUserNotificationCenter.current()
        center?.delegate = self
        Task { await refreshPermission() }
    }

    func activate(account: String) {
        activeAccount = account
        session = UUID().uuidString
        if seen[account] == nil { seen[account] = defaults?.stringArray(forKey: "notifications.seen.\(account)") ?? [] }
        if preferences.anyEnabled, center != nil { Task { await requestPermissionIfNeeded() } }
    }

    func deactivate() {
        let oldAccount = activeAccount
        activeAccount = nil
        pendingOpen = nil
        session = UUID().uuidString
        statusMessage = nil
        guard let center, let oldAccount else { return }
        Task {
            let pending = await center.pendingNotificationRequests()
            let delivered = await center.deliveredNotifications()
            let owns: (UNNotificationRequest) -> Bool = { request in
                request.identifier.hasPrefix(Self.prefix) && Self.shouldRemoveOnLogout(
                    notificationOwner: request.content.userInfo["owner"] as? String,
                    notificationSession: request.content.userInfo["session"] as? String,
                    loggedOutAccount: oldAccount, currentAccount: self.activeAccount, currentSession: self.session)
            }
            center.removePendingNotificationRequests(withIdentifiers: pending.filter(owns).map(\.identifier))
            center.removeDeliveredNotifications(withIdentifiers: delivered.map(\.request).filter(owns).map(\.identifier))
        }
    }

    func setPreference(_ keyPath: WritableKeyPath<NotificationPreferences, Bool>, _ enabled: Bool) {
        preferences[keyPath: keyPath] = enabled
        if enabled, preferences.anyEnabled { Task { await requestPermissionIfNeeded() } }
    }

    func refreshPermission() async {
        guard let center else { return }
        let settings = await center.notificationSettings()
        systemAlertsEnabled = settings.alertSetting == .enabled
        systemSoundEnabled = settings.soundSetting == .enabled
        switch settings.authorizationStatus {
        case .authorized, .provisional: permission = .allowed
        case .denied: permission = .denied
        case .notDetermined: permission = .unknown
        @unknown default: permission = .unavailable
        }
    }

    func requestPermissionIfNeeded() async {
        guard let center, !requestingPermission else { return }
        requestingPermission = true
        defer { requestingPermission = false }
        await refreshPermission()
        if permission == .unknown {
            do { _ = try await center.requestAuthorization(options: [.alert, .sound]) }
            catch { statusMessage = "Не удалось запросить разрешение macOS. Повторите в настройках уведомлений." }
            await refreshPermission()
        }
    }

    func enabled(_ kind: NotificationKind) -> Bool {
        switch kind {
        case .message: return preferences.messages
        case .steam: return preferences.steam
        case .download: return preferences.downloads
        case .game: return preferences.gameErrors
        case .test: return true
        }
    }

    @discardableResult
    func record(_ notice: ClientNotice) -> Bool {
        guard notice.owner == activeAccount else { return false }
        var ids = seen[notice.owner] ?? []
        guard !ids.contains(notice.fingerprint) else { return false }
        ids.append(notice.fingerprint)
        if ids.count > 1024 { ids.removeFirst(ids.count - 1024) }
        seen[notice.owner] = ids
        defaults?.set(ids, forKey: "notifications.seen.\(notice.owner)")
        return true
    }

    func post(_ notice: ClientNotice, alreadyRecorded: Bool = false) {
        guard notice.owner == activeAccount, alreadyRecorded || record(notice), enabled(notice.kind),
              canPresent?(notice.owner, notice.target) != false else { return }
        let visible = ClientNotice(id: notice.id, owner: notice.owner, kind: notice.kind,
            title: notice.title, body: preferences.previews ? notice.body : notice.privateBody,
            target: notice.target, timestamp: notice.timestamp)
        if let testDelivery { testDelivery(visible, preferences.sound); return }
        guard let center else { return }
        let expectedSession = session
        Task {
            await refreshPermission()
            guard permission == .allowed, activeAccount == notice.owner, session == expectedSession,
                  enabled(notice.kind), canPresent?(notice.owner, notice.target) != false else { return }
            let content = UNMutableNotificationContent()
            content.title = String(visible.title.prefix(180))
            content.body = String((preferences.previews ? notice.body : notice.privateBody).prefix(1000))
            if preferences.sound { content.sound = .default }
            content.threadIdentifier = "\(notice.owner)-\(notice.kind.rawValue)"
            content.userInfo = notice.target.fields.merging(["owner": notice.owner, "session": expectedSession, "kind": notice.kind.rawValue, "fingerprint": notice.fingerprint]) { first, _ in first }
            let request = UNNotificationRequest(identifier: Self.prefix + expectedSession + "." + notice.fingerprint, content: content, trigger: nil)
            do {
                try await center.add(request)
                if activeAccount != notice.owner || session != expectedSession {
                    center.removePendingNotificationRequests(withIdentifiers: [request.identifier])
                    center.removeDeliveredNotifications(withIdentifiers: [request.identifier])
                }
            } catch { statusMessage = "macOS не смогла показать уведомление. Проверьте системные настройки SkinOS." }
        }
    }

    static func shouldRemoveOnLogout(notificationOwner: String?, notificationSession: String?, loggedOutAccount: String, currentAccount: String?, currentSession: String) -> Bool {
        notificationOwner == loggedOutAccount && (currentAccount != loggedOutAccount || notificationSession != currentSession)
    }

    func sendTest() {
        guard let owner = activeAccount else { statusMessage = "Войдите в Steam, чтобы проверить уведомления SkinOS."; return }
        Task {
            await requestPermissionIfNeeded()
            guard permission == .allowed, owner == activeAccount else { return }
            post(ClientNotice(id: UUID().uuidString, owner: owner, kind: .test, title: "SkinOS · проверка уведомлений",
                body: "Это тестовое уведомление SkinOS. Сообщения друзьям не отправлялись.", target: .settings, timestamp: Date()))
            statusMessage = "Тестовое уведомление отправлено в macOS. Если баннера нет, проверьте режим фокусирования и настройки уведомлений."
        }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification, withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        Task { @MainActor [weak self] in
            guard let self, let owner = notification.request.content.userInfo["owner"] as? String,
                  owner == activeAccount, notification.request.content.userInfo["session"] as? String == session,
                  let target = NotificationTarget(fields: notification.request.content.userInfo),
                  let kind = (notification.request.content.userInfo["kind"] as? String).flatMap(NotificationKind.init(rawValue:)),
                  enabled(kind), canPresent?(owner, target) != false else { completionHandler([]); return }
            completionHandler(preferences.sound ? [.banner, .list, .sound] : [.banner, .list])
        }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping () -> Void) {
        Task { @MainActor [weak self] in
            defer { completionHandler() }
            guard let self, response.actionIdentifier == UNNotificationDefaultActionIdentifier,
                  response.notification.request.identifier.hasPrefix(Self.prefix),
                  let owner = response.notification.request.content.userInfo["owner"] as? String,
                  let target = NotificationTarget(fields: response.notification.request.content.userInfo) else { return }
            if let onOpen { onOpen(owner, target) } else { pendingOpen = (owner, target) }
        }
    }
}


@MainActor
final class SkinOSApplicationDelegate: NSObject, NSApplicationDelegate {
    weak var updates: AppUpdates?
    func applicationWillFinishLaunching(_ notification: Notification) { ClientNotifications.shared.start() }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        updates?.shouldAllowApplicationTermination() == false ? .terminateCancel : .terminateNow
    }
}
