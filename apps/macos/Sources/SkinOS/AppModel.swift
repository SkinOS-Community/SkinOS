import AppKit
import Foundation
import WebKit

@MainActor
final class AppModel: ObservableObject {
    @Published var section: AppSection = .library {
        didSet { markVisibleChatRead(); markVisibleNoticesRead() }
    }
    @Published var applicationIsActive = false
    @Published var applicationWindowVisible = false
    var launchNotificationIDs: [Int: String] = [:]
    @Published var noticeItems: [ClientNotice] = []
    @Published var noticesLoading = false
    @Published var noticesError: String?
    var notificationRequests: Set<String> = []
    var locallyReadNoticeIDs: Set<String> = []
    var notificationRouteWaiting: (owner: String, target: NotificationTarget)?
    let notifications: ClientNotifications
    weak var mainApplicationWindow: NSWindow?
    var activateApplication: () -> Void = { NSApplication.shared.activate(ignoringOtherApps: true) }

    @Published var filter: GameFilter = .all
    @Published var search = ""
    @Published var installFilter: InstallFilter = .all
    @Published var playFilter: PlayFilter = .all
    @Published var librarySort: LibrarySort = .name
    @Published var favoriteIDs: Set<Int> = []
    @Published var friends: [SteamFriend] = []
    @Published var presence: Presence = .invisible
    @Published var presencePending = false
    @Published var friendsLoading = false
    @Published var friendsLoaded = false
    @Published var friendsError: String?
    @Published var friendSearch = ""
    @Published var selectedFriendID: String?
    @Published var chats: [String: [ChatMessage]] = [:]
    @Published var chatLoading: Set<String> = []
    @Published var chatErrors: [String: String] = [:]
    @Published var unreadCounts: [String: Int] = [:]
    @Published var appDetails: [Int: StoreAppDetails] = [:]
    @Published var detailsLoading: Set<Int> = []
    @Published var detailsErrors: [Int: String] = [:]
    @Published var webSessionLoading = false
    @Published var webSessionError: String?
    @Published var webSessionReady = false
    @Published var webIdentity = UUID()
    @Published var storeURL = URL(string: "https://store.steampowered.com/")!
    @Published var profileURL = URL(string: "https://steamcommunity.com/my/")!
    var webCookies: [HTTPCookie] = []
    var websiteDataStore = WKWebsiteDataStore.nonPersistent()
    @Published var webSessionRevision = 0
    var socialRequests: [String: (command: String, target: String)] = [:]
    var detailRequests: [String: Int] = [:]
    var webSessionRequest: String?
    var webSessionRequests: [String: String] = [:]
    var webSessionExpiry: [String: TimeInterval] = [:]
    @Published var webSessionTargets: Set<String> = []
    var pendingChatRequests: [String: String] = [:]
    var requestedSocial = false

    @Published var account: SteamAccount?
    @Published var savedAccounts: [SteamAccount] = []
    @Published var games: [SteamGame] = []
    @Published var selectedGameID: Int?
    @Published var downloads: [Int: GameDownload] = [:]
    @Published var downloadOrder: [Int] = []
    @Published var maxConcurrentDownloads = 2
    @Published var downloadQueueReady = false
    @Published var downloadQueueLoading = false
    @Published var downloadQueueError: String?
    @Published var pendingDownloadControls: Set<Int> = []
    @Published var pendingEnqueueGames: Set<Int> = []
    @Published var downloadLimitPending = false
    var downloadQueueRevision: Int64 = -1
    var downloadControlRequests: [String: Int] = [:]
    var downloadQueueRequests: Set<String> = []
    var downloadLimitRequests: Set<String> = []
    @Published var launchStates: [Int: String] = [:]
    @Published var qrURL: String?
    @Published var guardRequired = false
    @Published var guardMessage = "Введите код Steam Guard"
    @Published var isAuthenticating = false
    @Published var isRefreshing = false
    @Published var coreReady = false
    @Published var steamConnected = false
    @Published var isReconnecting = false
    @Published var reconnectSeconds: Int?
    @Published var needsFreshLogin = false
    var hasAttemptedAutoLogin = false
    var hasAuthenticatedAccount = false
    var commandSink: ((String, [String: Any]) -> String?)?
    private var reconnectCountdown: Task<Void, Never>?
    @Published var authMessage = "Войдите в свой Steam-аккаунт"
    @Published var errorMessage: String?
    @Published var isShowingLogin = false
    @Published var libraryIsCached = false
    @Published var settings = ClientSettings()
    @Published var downloadRegions: [DownloadRegion] = []
    @Published var crossovers: [CrossOverInstallation] = []
    @Published var notProton = NotProtonVersion()
    @Published var gameSettings: [Int: GameSettings] = [:]
    @Published var gameForSettings: SteamGame?
    @Published var gameForLog: SteamGame?
    @Published var steamInputReports: [Int: SteamInputReport] = [:]
    @Published var steamInputErrors: [Int: String] = [:]
    @Published var steamInputMessages: [Int: String] = [:]
    @Published var steamInputFeedbackErrors: [Int: String] = [:]
    @Published var steamInputRequests: [String: SteamInputRequest] = [:]
    @Published var uninstallingGames: Set<Int> = []
    @Published var uninstallNotices: [Int: String] = [:]
    var uninstallRequests: [String: Int] = [:]
    @Published var uninstallPlans: [Int: UninstallPlan] = [:]
    @Published var loadingUninstallPlans: Set<Int> = []
    var uninstallPlanRequests: [String: Int] = [:]
    @Published var gameLogs: [Int: GameLaunchLog] = [:]
    @Published var gameLogErrors: [Int: String] = [:]
    @Published var loadingGameLogs: Set<Int> = []
    var gameLogRequests: [String: Int] = [:]
    var latestGameLogRequests: [Int: String] = [:]
    @Published var settingsLoaded = false
    @Published var isSavingSettings = false
    @Published var isSavingGameSettings = false
    @Published var loadingGameSettings: Set<Int> = []
    @Published var isCheckingUpdates = false
    @Published var settingsNotice: String?
    @Published var gameSettingsNotice: String?
    @Published var updatesNotice: String?
    @Published var launchPhases: [Int: String] = [:]
    private let connection = CoreConnection()
    private var started = false
    private var stopping = false
    var gameRequests: [String: Int] = [:]
    var requestCommands: [String: String] = [:]
    private var settingsRequest: String?
    private var gameSettingsRequest: String?
    private var updateRequests: Set<String> = []

    init(notifications: ClientNotifications? = nil) {
        self.notifications = notifications ?? ClientNotifications()
        self.notifications.onOpen = { [weak self] owner, target in self?.openNotification(owner: owner, target: target) }
        self.notifications.canPresent = { [weak self] owner, target in self?.canPresentNotification(owner: owner, target: target) ?? false }
    }

    var selectedGame: SteamGame? { games.first { $0.id == selectedGameID } }
    var activeDownloads: Int { downloads.values.filter(\.isActive).count }
    var filteredGames: [SteamGame] {
        games.filter { game in
            (search.isEmpty || game.name.localizedCaseInsensitiveContains(search))
            && (filter != .mac || game.supportsMacOS)
            && (filter != .windows || game.supportsWindows || game.installedPlatform == "windows")
            && (filter != .linux || game.supportsLinux)
            && (installFilter != .installed || game.isInstalled)
            && (installFilter != .uninstalled || !game.isInstalled)
            && (installFilter != .updates || game.updateAvailable)
            && (playFilter != .played || game.playtimeMinutes > 0)
            && (playFilter != .unplayed || game.playtimeMinutes == 0)
            && (playFilter != .favorites || favoriteIDs.contains(game.id))
        }.sorted {
            switch librarySort {
            case .recent where $0.lastPlayed != $1.lastPlayed: return $0.lastPlayed > $1.lastPlayed
            case .playtime where $0.playtimeMinutes != $1.playtimeMinutes: return $0.playtimeMinutes > $1.playtimeMinutes
            default: return $0.name.localizedStandardCompare($1.name) == .orderedAscending
            }
        }
    }

    func start() {
        guard !started else { return }
        started = true
        stopping = false
        notifications.start()
        connection.onEvent = { [weak self] event in self?.receive(event) }
        connection.onExit = { [weak self] code in
            guard let self, !self.stopping else { return }
            self.coreReady = false
            self.steamInputConnectionLost()
            self.isAuthenticating = false
            self.isRefreshing = false
            self.isCheckingUpdates = false
            self.isSavingSettings = false
            self.isSavingGameSettings = false
            self.loadingGameSettings = []
            for appID in self.loadingGameLogs {
                self.gameLogErrors[appID] = "Соединение с SkinOS Core прервано."
            }
            self.uninstallingGames = []
            self.uninstallRequests = [:]
            self.uninstallPlans = [:]
            self.loadingUninstallPlans = []
            self.uninstallPlanRequests = [:]
            self.downloadQueueLoading = false
            self.pendingDownloadControls = []
            self.pendingEnqueueGames = []
            self.downloadLimitPending = false
            self.downloadControlRequests = [:]
            self.downloadLimitRequests = []
            self.loadingGameLogs = []
            self.gameLogRequests = [:]
            self.latestGameLogRequests = [:]
            for appID in self.downloads.keys where self.downloads[appID]?.isActive == true {
                self.downloads[appID]?.failed = true
                self.downloads[appID]?.message = "Соединение прервано. Повторите загрузку после перезапуска."
            }
            self.launchPhases = self.launchPhases.mapValues { _ in "failed" }
            self.errorMessage = "SkinOS Core завершился (код \(code)). Перезапустите приложение."
        }
        do { try connection.start() } catch { errorMessage = error.localizedDescription }
    }

    func shutdown() { stopping = true; reconnectCountdown?.cancel(); connection.stop() }

    @discardableResult
    func send(_ command: String, payload: [String: Any] = [:]) -> String? {
        let sentID = commandSink.map { $0(command, payload) } ?? connection.send(command, payload: payload)
        guard let id = sentID else {
            errorMessage = "Нет соединения с SkinOS Core. Перезапустите приложение."
            isAuthenticating = false
            isRefreshing = false
            return nil
        }
        requestCommands[id] = command
        return id
    }

    func startQRLogin() {
        cancelReconnectDisplay(); hasAttemptedAutoLogin = true; needsFreshLogin = false
        errorMessage = nil
        qrURL = nil
        guardRequired = false
        isShowingLogin = true
        isAuthenticating = true
        authMessage = "Готовим защищённый вход…"
        send("login_qr")
    }

    func passwordLogin(accountName: String, password: String) {
        cancelReconnectDisplay(); hasAttemptedAutoLogin = true; needsFreshLogin = false
        errorMessage = nil
        guardRequired = false
        isAuthenticating = true
        authMessage = "Подключаемся к Steam…"
        send("login_password", payload: ["account_name": accountName, "password": password])
    }

    func submitGuard(_ code: String) {
        authMessage = "Проверяем Steam Guard…"
        send("submit_guard", payload: ["code": code])
    }

    func cancelAuth(dismiss: Bool = true) {
        if isAuthenticating || isReconnecting { send("cancel_auth") }
        cancelReconnectDisplay(); hasAttemptedAutoLogin = true
        isAuthenticating = false
        guardRequired = false
        qrURL = nil
        if dismiss { isShowingLogin = false }
    }

    func refreshLibrary() {
        guard account != nil else { isShowingLogin = true; return }
        isRefreshing = true
        send("refresh_library")
    }

    func restore(_ savedAccount: SteamAccount) {
        cancelReconnectDisplay(); hasAttemptedAutoLogin = true; needsFreshLogin = false; hasAuthenticatedAccount = false; steamConnected = false
        errorMessage = nil
        resetCommunity(preserveNotificationRoute: true)
        account = nil
        games = []
        selectedGameID = nil
        isAuthenticating = true
        authMessage = "Подключаем \(savedAccount.displayName)…"
        send("restore_session", payload: ["steamid": savedAccount.id])
    }

    func logout() {
        cancelReconnectDisplay(); hasAttemptedAutoLogin = true; needsFreshLogin = false; hasAuthenticatedAccount = false; steamConnected = false
        send("logout")
        resetCommunity()
        account = nil
        games = []
        selectedGameID = nil
        qrURL = nil
        guardRequired = false
        isAuthenticating = false
        authMessage = "Войдите в свой Steam-аккаунт"
    }

    func retryConnection() {
        guard !steamConnected else { return }
        if needsFreshLogin { startQRLogin(); return }
        reconnectCountdown?.cancel(); reconnectSeconds = 0; isReconnecting = true
        send("retry_connection")
    }

    func cancelReconnectDisplay() {
        reconnectCountdown?.cancel(); reconnectCountdown = nil
        reconnectSeconds = nil; isReconnecting = false
    }

    func showReconnectCountdown(_ seconds: Int?) {
        reconnectCountdown?.cancel()
        reconnectSeconds = seconds
        guard let seconds, seconds > 0 else { return }
        reconnectCountdown = Task { [weak self] in
            for remaining in stride(from: seconds - 1, through: 0, by: -1) {
                do { try await Task.sleep(nanoseconds: 1_000_000_000) } catch { return }
                guard let self, self.isReconnecting else { return }
                self.reconnectSeconds = remaining
            }
        }
    }

    func loadSettings() { send("get_settings") }

    func saveSettings(_ value: ClientSettings) {
        errorMessage = nil
        settingsNotice = nil
        isSavingSettings = true
        settingsRequest = send("save_settings", payload: ["settings": value.payload])
        if settingsRequest == nil { isSavingSettings = false }
    }

    func showSettings(for game: SteamGame) {
        gameSettingsNotice = nil
        gameForSettings = game
        loadingGameSettings.insert(game.id)
        if let requestID = send("get_game_settings", payload: ["appid": game.id]) {
            gameRequests[requestID] = game.id
        } else { loadingGameSettings.remove(game.id) }
    }

    func saveGameSettings(_ value: GameSettings, for game: SteamGame) {
        errorMessage = nil
        gameSettingsNotice = nil
        isSavingGameSettings = true
        gameSettingsRequest = send("save_game_settings", payload: ["appid": game.id, "settings": value.payload])
        if let gameSettingsRequest { gameRequests[gameSettingsRequest] = game.id }
        else { isSavingGameSettings = false }
    }

    func checkUpdates(for game: SteamGame? = nil) {
        errorMessage = nil
        updatesNotice = nil
        isCheckingUpdates = true
        if let requestID = send("check_updates", payload: game.map { ["appid": $0.id] } ?? [:]) {
            updateRequests.insert(requestID)
        } else { isCheckingUpdates = false }
    }

    func canInstall(_ game: SteamGame) -> Bool {
        game.supportsMacOS || game.supportsWindows || game.installedPlatform == "windows"
    }

    func launchButtonTitle(_ game: SteamGame) -> String {
        if launchPhases[game.id] == "running" { return "Игра запущена" }
        if launchPhases[game.id] == "finishing" { return "Завершаем…" }
        return launchIsActive(game) ? "Запуск…" : "Играть"
    }

    func launchIsActive(_ game: SteamGame) -> Bool {
        ["authenticating", "authenticated", "starting", "running", "stopping", "finishing"].contains(launchPhases[game.id] ?? "")
    }

    func stopGame(_ game: SteamGame) {
        guard launchIsActive(game), !["stopping", "finishing"].contains(launchPhases[game.id] ?? "") else { return }
        if let id = send("stop_game", payload: ["appid": game.id]) {
            gameRequests[id] = game.id
            launchPhases[game.id] = "stopping"
            launchStates[game.id] = "Завершаем игру…"
        }
    }

    func install(_ game: SteamGame, update: Bool = false) {
        guard account != nil else { isShowingLogin = true; return }
        guard coreReady else { errorMessage = "Нет соединения с SkinOS Core. Перезапустите приложение."; return }
        guard !pendingEnqueueGames.contains(game.id), !uninstallingGames.contains(game.id) else { return }
        errorMessage = nil
        pendingEnqueueGames.insert(game.id)
        if !downloadQueueReady {
            downloads[game.id] = GameDownload(id: game.id, phase: "queued", isUpdate: update)
            if !downloadOrder.contains(game.id) { downloadOrder.append(game.id) }
        }
        if let id = send(update ? "update_game" : "install_game", payload: ["appid": game.id]) {
            gameRequests[id] = game.id
        } else {
            pendingEnqueueGames.remove(game.id)
            if !downloadQueueReady { downloads[game.id]?.failed = true }
        }
    }

    func launch(_ game: SteamGame) {
        guard !launchIsActive(game) else { return }
        guard !steamInputBusy else { errorMessage = "Дождитесь завершения проверки Steam Input."; return }
        guard !uninstallingGames.contains(game.id) else { return }
        guard account != nil else { isShowingLogin = true; return }
        guard coreReady else { errorMessage = "Нет соединения с SkinOS Core. Перезапустите приложение."; return }
        errorMessage = nil
        steamInputErrors[game.id] = nil
        steamInputMessages[game.id] = nil
        steamInputFeedbackErrors[game.id] = nil
        launchStates[game.id] = "Подготовка запуска…"
        launchPhases[game.id] = "starting"
        if let id = send("launch_game", payload: ["appid": game.id]) {
            gameRequests[id] = game.id
            launchNotificationIDs[game.id] = id
        }
        else { launchPhases[game.id] = "failed" }
    }

    func receive(_ envelope: [String: Any]) {
        guard let event = envelope["event"] as? String else { return }
        let payload = envelope["payload"] as? [String: Any] ?? [:]
        let requestID = envelope["id"] as? String
        if receiveSteamInput(event, payload: payload, requestID: requestID) { return }
        if receiveNotificationEvent(event, payload: payload, requestID: requestID) {
            if let requestID { requestCommands.removeValue(forKey: requestID) }
            return
        }
        if receiveUninstall(event, payload: payload, requestID: requestID) {
            if let requestID, event != "game_uninstalled" || uninstallRequests[requestID] == nil { requestCommands.removeValue(forKey: requestID) }
            return
        }
        if receiveDownloadQueue(event, payload: payload, requestID: requestID) {
            if let requestID { requestCommands.removeValue(forKey: requestID) }
            return
        }
        if receiveLaunchLog(event, payload: payload, requestID: requestID) {
            if let requestID { requestCommands.removeValue(forKey: requestID) }
            return
        }
        if receiveCommunity(event, payload: payload, requestID: requestID) {
            if let requestID { requestCommands.removeValue(forKey: requestID) }
            return
        }
        switch event {
        case "ready":
            coreReady = true
            send("status")
            loadSettings()
        case "settings":
            let newSettings = ClientSettings(json: payload["settings"] as? [String: Any] ?? payload)
            let newCrossovers = (payload["crossovers"] as? [[String: Any]] ?? []).compactMap(CrossOverInstallation.init(json:))
            let newNotProton = NotProtonVersion(json: payload["notproton"] as? [String: Any] ?? [:])
            if !settings.hasSameRuntimeSettings(as: newSettings) || notProton != newNotProton
                || crossovers.sorted(by: { $0.path < $1.path }) != newCrossovers.sorted(by: { $0.path < $1.path }) {
                invalidateSteamInputReports()
            }
            settings = newSettings; crossovers = newCrossovers; notProton = newNotProton
            if let regions = payload["download_regions"] as? [[String: Any]] {
                downloadRegions = DownloadRegion.catalog(json: regions)
            }
            settingsLoaded = true
            if let requestID, requestID == settingsRequest {
                isSavingSettings = false
                settingsNotice = "Настройки сохранены"
                settingsRequest = nil
            }
            if let requestID { requestCommands.removeValue(forKey: requestID) }
        case "game_settings":
            guard let appID = payload["appid"] as? Int else { return }
            let newSettings = GameSettings(json: payload["settings"] as? [String: Any] ?? payload)
            if (gameSettings[appID] ?? GameSettings()) != newSettings { invalidateSteamInputReports(for: appID) }
            gameSettings[appID] = newSettings
            loadingGameSettings.remove(appID)
            if let requestID, requestID == gameSettingsRequest {
                isSavingGameSettings = false
                gameSettingsNotice = "Настройки сохранены. Применятся при следующем запуске."
                gameSettingsRequest = nil
            }
            if let requestID { requestCommands.removeValue(forKey: requestID); gameRequests.removeValue(forKey: requestID) }
        case "updates":
            let updates = payload["games"] as? [[String: Any]] ?? []
            for item in updates {
                guard let appID = item["appid"] as? Int, let index = games.firstIndex(where: { $0.id == appID }) else { continue }
                games[index].updateAvailable = item["update_available"] as? Bool ?? false
                games[index].installedBuildID = item["installed_build_id"] as? String ?? games[index].installedBuildID
                games[index].availableBuildID = item["available_build_id"] as? String ?? games[index].availableBuildID
            }
            let count = updates.filter { $0["update_available"] as? Bool == true }.count
            updatesNotice = updates.isEmpty ? "Нет установленных игр для проверки" : count == 0 ? "Установлены последние версии" : "Доступно обновлений: \(count)"
            if let requestID { updateRequests.remove(requestID) }
            isCheckingUpdates = !updateRequests.isEmpty
            if let requestID { requestCommands.removeValue(forKey: requestID) }
        case "status":
            coreReady = true
            if let accounts = payload["accounts"] as? [[String: Any]] {
                savedAccounts = accounts.compactMap { item in
                    guard let id = item["steamid"] as? String, let name = item["account_name"] as? String else { return nil }
                    return SteamAccount(id: id, accountName: name,
                        displayName: item["display_name"] as? String ?? name,
                        avatarURL: (item["avatar_url"] as? String).flatMap(URL.init(string:)))
                }
            }
            if let state = payload["state"] as? String {
                switch state {
                case "connecting":
                    showReconnectCountdown(nil)
                    steamConnected = false; isAuthenticating = true; isRefreshing = true
                    isReconnecting = payload["reconnecting"] as? Bool ?? false
                    authMessage = isReconnecting ? "Восстанавливаем подключение к Steam…" : "Подключаемся к Steam…"
                    if account == nil, let row = payload["account"] as? [String: Any],
                       let steamID = row["steamid"] as? String, let name = row["account_name"] as? String {
                        account = SteamAccount(id: steamID, accountName: name, displayName: name, avatarURL: nil)
                    }
                case "loading_library": isRefreshing = true
                case "signed_out":
                    cancelReconnectDisplay(); steamConnected = false; hasAuthenticatedAccount = false
                    resetCommunity(preserveNotificationRoute: account == nil)
                    account = nil
                    games = []
                    selectedGameID = nil
                    qrURL = nil
                    guardRequired = false
                    downloads = [:]
                    launchStates = [:]
                    launchPhases = [:]
                    gameRequests = [:]
                    updateRequests = []
                    isCheckingUpdates = false
                    libraryIsCached = false
                    isRefreshing = false
                    isAuthenticating = false
                    authMessage = "Войдите в свой Steam-аккаунт"
                case "disconnected", "reconnecting":
                    steamConnected = false
                    isReconnecting = state == "reconnecting" || payload["retry_in_seconds"] is NSNumber
                    isAuthenticating = false; isRefreshing = false
                    libraryIsCached = true
                    showReconnectCountdown((payload["retry_in_seconds"] as? NSNumber)?.intValue)
                    authMessage = isReconnecting ? "Соединение со Steam прервано. Подключимся автоматически." : "Подключение к Steam остановлено."
                case "auth_required":
                    cancelReconnectDisplay(); steamConnected = false; needsFreshLogin = true
                    isAuthenticating = false; isRefreshing = false; libraryIsCached = true
                    authMessage = "Сеанс Steam завершён. Войдите заново."
                    errorMessage = nil
                case "authenticated":
                    steamConnected = true
                    cancelReconnectDisplay()
                default: break
                }
            }
            if let status = payload["message"] as? String { authMessage = status }
            if !hasAttemptedAutoLogin, requestID.flatMap({ requestCommands[$0] }) == "status" {
                hasAttemptedAutoLogin = true
                if payload["state"] as? String == "signed_out", payload["auto_login_disabled"] as? Bool != true, !savedAccounts.isEmpty {
                    isAuthenticating = true
                    authMessage = "Подключаем последний аккаунт Steam…"
                    send("auto_login")
                }
            }
        case "auth_qr":
            qrURL = payload["url"] as? String
            isAuthenticating = true
            authMessage = "Отсканируйте QR-код в мобильном приложении Steam"
        case "guard_required":
            let type = payload["kind"] as? String ?? payload["type"] as? String ?? "code"
            guardRequired = !type.contains("confirmation")
            guardMessage = payload["message"] as? String ?? (type.contains("confirmation")
                ? "Подтвердите вход в мобильном приложении Steam" : "Введите код Steam Guard")
            authMessage = guardMessage
        case "authenticated":
            let name = payload["account_name"] as? String ?? "Steam"
            let steamID = payload["steamid"] as? String ?? ""
            if !hasAuthenticatedAccount || account?.id != steamID { resetCommunity(preserveNotificationRoute: true) }
            else { requestedSocial = false; friendsLoading = false; socialRequests = [:]; presencePending = false }
            hasAuthenticatedAccount = true; steamConnected = true; needsFreshLogin = false
            cancelReconnectDisplay()
            account = SteamAccount(id: steamID, accountName: name,
                displayName: payload["display_name"] as? String ?? name,
                avatarURL: (payload["avatar_url"] as? String).flatMap(URL.init(string:)))
            isAuthenticating = false
            guardRequired = false
            qrURL = nil
            isShowingLogin = false
            authMessage = "Подключено к Steam"
            errorMessage = nil
            isRefreshing = true
            notifications.activate(account: steamID)
            resumeNotificationRouteIfPossible()
            favoriteIDs = Set(UserDefaults.standard.array(forKey: "favorites.\(steamID)") as? [Int] ?? [])
            profileURL = URL(string: "https://steamcommunity.com/profiles/\(steamID)/")!
            // The backend loads the library after authentication; a second request
            // here races that load and produces a library_busy error.
        case "library":
            games = (payload["games"] as? [[String: Any]] ?? []).compactMap(SteamGame.init(json:))
            libraryIsCached = payload["cached"] as? Bool ?? false
            if !libraryIsCached { loadCommunity() }
            isRefreshing = libraryIsCached && isAuthenticating
            if let selectedGameID, !games.contains(where: { $0.id == selectedGameID }) { self.selectedGameID = nil }
        case "install_progress":
            guard !downloadQueueReady else { return }
            guard let appID = payload["appid"] as? Int else { return }
            var download = downloads[appID] ?? GameDownload(id: appID)
            download.phase = payload["phase"] as? String ?? download.phase
            download.message = payload["message"] as? String
            download.downloadedBytes = (payload["downloaded_bytes"] as? NSNumber)?.int64Value ?? download.downloadedBytes
            download.totalBytes = (payload["total_bytes"] as? NSNumber)?.int64Value ?? download.totalBytes
            download.bytesPerSecond = (payload["bytes_per_second"] as? NSNumber)?.doubleValue ?? 0
            download.etaSeconds = (payload["eta_seconds"] as? NSNumber)?.intValue
            download.reusedBytes = (payload["reused_bytes"] as? NSNumber)?.int64Value ?? download.reusedBytes
            download.processedBytes = (payload["processed_bytes"] as? NSNumber)?.int64Value ?? download.processedBytes
            download.installTotalBytes = (payload["install_total_bytes"] as? NSNumber)?.int64Value ?? download.installTotalBytes
            download.failed = false
            if let value = (payload["progress"] as? NSNumber)?.doubleValue {
                download.progress = min(1, max(0, value > 1 ? value / 100 : value))
            } else if download.totalBytes > 0 {
                download.progress = min(1, Double(download.downloadedBytes) / Double(download.totalBytes))
            }
            downloads[appID] = download
        case "install_complete":
            guard let appID = payload["appid"] as? Int else { return }
            var download = downloads[appID] ?? GameDownload(id: appID)
            download.complete = true; download.failed = false; download.progress = 1; download.phase = "Готово"
            download.bytesPerSecond = 0; download.etaSeconds = 0
            if !downloadQueueReady { downloads[appID] = download }
            if let index = games.firstIndex(where: { $0.id == appID }) {
                games[index].installedDirectory = payload["install_dir"] as? String ?? games[index].installedDirectory
                games[index].updateAvailable = false
            }
        case "launch_status":
            guard let appID = payload["appid"] as? Int else { return }
            let state = payload["state"] as? String ?? "unknown"
            if state == "exited", let requestID, requestCommands[requestID] == "launch_game",
               gameRequests[requestID] == appID {
                launchPhases[appID] = "finishing"
                launchStates[appID] = "Игра закрыта. Завершаем сеанс…"
                break
            }
            launchPhases[appID] = state
            let labels = ["authenticating": "Подключаем аккаунт…", "authenticated": "Аккаунт подключён", "starting": "Запускаем игру…", "running": "Игра запущена", "stopping": "Завершаем игру…", "exited": "Игра закрыта", "failed": "Не удалось запустить игру"]
            launchStates[appID] = payload["message"] as? String ?? labels[state] ?? state
            if state == "failed" { notifyLaunchFailure(appID) }
        case "operation_complete":
            if let requestID, let appID = gameRequests[requestID] {
                if ["install_game", "update_game"].contains(requestCommands[requestID] ?? "") { pendingEnqueueGames.remove(appID) }
                if requestCommands[requestID] == "launch_game", launchPhases[appID] != "failed" {
                    launchPhases[appID] = "exited"
                    launchStates[appID] = "Игра закрыта"
                }
                gameRequests.removeValue(forKey: requestID)
                requestCommands.removeValue(forKey: requestID)
            }
        case "error":
            let code = payload["code"] as? String ?? "unknown"
            if ["auth_required", "no_session", "session_not_found"].contains(code) {
                cancelReconnectDisplay(); needsFreshLogin = true; steamConnected = false
                authMessage = "Войдите в Steam заново через QR-код"
                errorMessage = nil
            } else if ["not_authenticated"].contains(code), account == nil {
                authMessage = "Войдите в свой Steam-аккаунт"
            } else {
                errorMessage = payload["message"] as? String ?? "Steam не смог выполнить запрос."
            }
            isAuthenticating = false
            isRefreshing = false
            if requestID == settingsRequest { isSavingSettings = false; settingsRequest = nil }
            if requestID == gameSettingsRequest { isSavingGameSettings = false; gameSettingsRequest = nil }
            if let requestID { updateRequests.remove(requestID) }
            isCheckingUpdates = !updateRequests.isEmpty
            if let appID = payload["appid"] as? Int ?? requestID.flatMap({ gameRequests[$0] }) {
                loadingGameSettings.remove(appID)
                let command = requestID.flatMap { requestCommands[$0] } ?? ""
                if ["install_game", "update_game"].contains(command) { pendingEnqueueGames.remove(appID) }
                if ["install_game", "update_game"].contains(command), !downloadQueueReady, var download = downloads[appID], !download.complete {
                    download.failed = true
                    download.message = errorMessage
                    downloads[appID] = download
                }
                if command == "stop_game" {
                    if launchPhases[appID] == "stopping" {
                        launchStates[appID] = errorMessage ?? "Не удалось завершить игру"
                        launchPhases[appID] = "running"
                    }
                }
                if command == "launch_game" {
                    launchStates[appID] = errorMessage ?? "Не удалось запустить игру"
                    launchPhases[appID] = "failed"
                    notifyLaunchFailure(appID)
                }
            }
            if let requestID { gameRequests.removeValue(forKey: requestID); requestCommands.removeValue(forKey: requestID) }
        default: break
        }
    }
}
