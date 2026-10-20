import Foundation
import SwiftUI

struct SteamInputDevice: Identifiable, Equatable {
    let id: String
    var number: Int
    var gamepadIndex: Int?
    var bindingRevisionAvailable: Bool
    var bindingMajor: Int?
    var bindingMinor: Int?
    var xinputSlot: Int?
    var routeObserved: Bool

    var bindingRevisionDescription: String {
        guard bindingRevisionAvailable, let major = bindingMajor, let minor = bindingMinor else {
            return "Ревизия не предоставлена"
        }
        return "Ревизия раскладки API: \(major).\(minor)"
    }

    var mappingDescription: String {
        if routeObserved, let slot = xinputSlot { return "Steam Input → XInput \(slot + 1)" }
        if let gamepadIndex { return "Индекс геймпада: \(gamepadIndex + 1) · XInput не подтверждён" }
        return "Сопоставление с XInput не обнаружено"
    }
}

struct SteamInputReport: Equatable {
    let appID: Int
    let fixture: Bool
    let apiInitialized: Bool
    let identityVerified: Bool
    let scanCompleted: Bool
    let controllerSeen: Bool
    let maxControllers: Int
    let xinputRouteObserved: Bool
    let activityVerified: Bool
    let physicalDeviceVerified: Bool
    let devices: [SteamInputDevice]
    let elapsedMilliseconds: Int?

    init?(json: [String: Any]) {
        guard let appID = json["appid"] as? Int,
              let fixture = json["fixture"] as? Bool,
              let summary = json["summary"] as? [String: Any],
              let apiInitialized = summary["api_initialized"] as? Bool,
              let identityVerified = summary["identity_verified"] as? Bool,
              let scanCompleted = summary["scan_completed"] as? Bool,
              let controllerSeen = summary["controller_seen"] as? Bool,
              let count = summary["max_controllers"] as? Int, (0...16).contains(count),
              controllerSeen == (count > 0) else { return nil }
        self.appID = appID
        self.fixture = fixture
        self.apiInitialized = apiInitialized
        self.identityVerified = identityVerified
        self.scanCompleted = scanCompleted
        self.controllerSeen = controllerSeen
        maxControllers = count
        xinputRouteObserved = summary["xinput_route_observed"] as? Bool ?? false
        activityVerified = summary["steam_input_activity_verified"] as? Bool ?? false
        // A fixture cannot prove anything about a physical device, even if a
        // malformed report incorrectly claims that it did.
        physicalDeviceVerified = !fixture && apiInitialized && identityVerified && scanCompleted && controllerSeen
            && activityVerified && (summary["physical_device_verified"] as? Bool ?? false)
        elapsedMilliseconds = summary["elapsed_ms"] as? Int
        var observed: [String: SteamInputDevice] = [:]
        for sample in (json["samples"] as? [[String: Any]] ?? []).prefix(1200)
            where sample["event"] as? String == "steam_input_snapshot" {
            let routes = sample["xinput"] as? [[String: Any]] ?? []
            for controller in (sample["controllers"] as? [[String: Any]] ?? []).prefix(16) {
                guard let handle = controller["handle"] as? String, !handle.isEmpty, handle != "0" else { continue }
                let index = (controller["gamepad_index"] as? Int).flatMap { (0...3).contains($0) ? $0 : nil }
                let route = routes.first { $0["steam_input_handle"] as? String == handle && $0["routed"] as? Bool == true }
                let slot = (route?["slot"] as? Int).flatMap { (0...3).contains($0) ? $0 : nil }
                let old = observed[handle]
                // Historical reports used an imprecise name for this API result.
                let revisionAvailable = controller["binding_revision_available"] as? Bool
                    ?? controller["binding_loaded"] as? Bool ?? false
                observed[handle] = SteamInputDevice(id: handle, number: old?.number ?? observed.count + 1,
                    gamepadIndex: index, bindingRevisionAvailable: revisionAvailable,
                    bindingMajor: revisionAvailable ? controller["binding_major"] as? Int : nil,
                    bindingMinor: revisionAvailable ? controller["binding_minor"] as? Int : nil,
                    xinputSlot: slot ?? old?.xinputSlot, routeObserved: slot != nil || old?.routeObserved == true)
            }
        }
        devices = observed.values.sorted { $0.number < $1.number }
    }

    var controllerDescription: String {
        if fixture { return "Тестовые данные · контроллеров: \(maxControllers)" }
        return maxControllers == 0 ? "Контроллеры не подключены" : "Во время проверки найдено: \(maxControllers)"
    }
}

struct SteamInputEnvironment: Equatable {
    let client: ClientSettings
    let game: GameSettings
    let crossovers: [CrossOverInstallation]
    let notProton: NotProtonVersion

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.client.hasSameRuntimeSettings(as: rhs.client) && lhs.game == rhs.game
            && lhs.crossovers == rhs.crossovers && lhs.notProton == rhs.notProton
    }
}

struct SteamInputRequest {
    let appID: Int
    let accountID: String
    let environment: SteamInputEnvironment
    var invalidated = false
}

@MainActor
extension AppModel {
    var steamInputBusy: Bool { !steamInputRequests.isEmpty }
    var anyGameActive: Bool {
        launchPhases.values.contains { ["authenticating", "authenticated", "starting", "running", "stopping", "finishing"].contains($0) }
    }

    func supportsSteamInputDiagnostic(_ game: SteamGame) -> Bool {
        game.isInstalled && (game.installedPlatform == "windows"
            || gameSettings[game.id]?.compatibility == "notproton"
            || !game.supportsMacOS && game.supportsWindows)
    }

    func steamInputBlockReason(for game: SteamGame) -> String? {
        if !coreReady || !steamConnected || account == nil { return "Для проверки подключитесь к Steam." }
        if !supportsSteamInputDiagnostic(game) { return "Проверка доступна для установленной Windows-игры через NotProton." }
        if loadingGameSettings.contains(game.id) { return "Загружаем настройки игры…" }
        if launchPhases.values.contains("finishing") { return "Дождитесь завершения игрового сеанса." }
        if anyGameActive { return "Закройте запущенную игру перед проверкой." }
        if steamInputBusy { return "Дождитесь завершения текущей проверки." }
        if uninstallingGames.contains(game.id) || downloads[game.id]?.isActive == true { return "Дождитесь завершения операции с файлами игры." }
        return nil
    }

    func steamInputEnvironment(for appID: Int) -> SteamInputEnvironment {
        SteamInputEnvironment(client: settings, game: gameSettings[appID] ?? GameSettings(),
            crossovers: crossovers.sorted { $0.path < $1.path }, notProton: notProton)
    }

    func invalidateSteamInputReports(for appID: Int? = nil) {
        if let appID {
            steamInputReports[appID] = nil
            steamInputErrors[appID] = nil
            steamInputFeedbackErrors[appID] = nil
        } else {
            steamInputReports = [:]
            steamInputErrors = [:]
            steamInputFeedbackErrors = [:]
        }
        // Keep the runtime reservation until the original operation drains.
        // Even changing settings back cannot make an in-flight result current.
        for id in Array(steamInputRequests.keys) {
            guard var request = steamInputRequests[id], appID == nil || request.appID == appID else { continue }
            request.invalidated = true
            steamInputRequests[id] = request
            steamInputErrors[request.appID] = "Настройки изменились. Требуется новая проверка Steam Input."
        }
    }

    func diagnoseSteamInput(for game: SteamGame) {
        guard steamInputBlockReason(for: game) == nil, let account else { return }
        steamInputErrors[game.id] = nil
        steamInputReports[game.id] = nil
        steamInputMessages[game.id] = "Подготовка проверки…"
        if let id = send("diagnose_input", payload: ["appid": game.id]) {
            steamInputRequests[id] = SteamInputRequest(appID: game.id, accountID: account.id, environment: steamInputEnvironment(for: game.id))
        } else {
            steamInputErrors[game.id] = "Нет соединения с SkinOS Core. Повторите после подключения."
            steamInputMessages[game.id] = nil
        }
    }

    func resetSteamInputDiagnostics() {
        for id in steamInputRequests.keys { requestCommands.removeValue(forKey: id) }
        steamInputRequests = [:]; steamInputReports = [:]
        steamInputErrors = [:]; steamInputMessages = [:]
        steamInputFeedbackErrors = [:]
    }

    func steamInputConnectionLost() {
        steamInputFeedbackErrors = [:]
        for request in steamInputRequests.values {
            steamInputErrors[request.appID] = "Соединение с SkinOS Core прервано. Повторите проверку после перезапуска."
            steamInputMessages[request.appID] = nil
        }
        for id in steamInputRequests.keys { requestCommands.removeValue(forKey: id) }
        steamInputRequests = [:]
    }

    func receiveSteamInput(_ event: String, payload: [String: Any], requestID: String?) -> Bool {
        if event == "steam_input_rumble_status" {
            guard let requestID, requestCommands[requestID] == "launch_game",
                  let appID = gameRequests[requestID], payload["appid"] as? Int == appID,
                  let owner = account?.id, payload["steamid"] as? String == owner else { return true }
            switch payload["state"] as? String {
            case "failed":
                steamInputFeedbackErrors[appID] = "Вибрация недоступна. Подробности смотрите в журнале запуска."
            case "ready", "reconnected":
                steamInputFeedbackErrors[appID] = nil
            default: break
            }
            // Feedback does not consume Play/Stop or change the game's phase.
            return true
        }
        if event == "steam_input_status", let requestID,
           requestCommands[requestID] == "launch_game", let appID = gameRequests[requestID],
           payload["appid"] as? Int == appID, let owner = account?.id,
           payload["steamid"] as? String == owner {
            if payload["state"] as? String == "failed" {
                let message = payload["message"] as? String ?? "Steam Input отключён. Игра продолжает работать."
                steamInputErrors[appID] = message
                steamInputMessages[appID] = nil
                errorMessage = "Steam Input: \(message)"
            } else {
                if payload["state"] as? String == "reconnected",
                   let previous = steamInputErrors.removeValue(forKey: appID),
                   errorMessage == "Steam Input: \(previous)" {
                    errorMessage = nil
                }
                steamInputMessages[appID] = payload["message"] as? String
            }
            // Keep the launch request and phase intact: the game still owns
            // its independent Play/Stop lifecycle, even when input degrades.
            return true
        }
        let isInputEvent = event == "steam_input_status" || event == "steam_input_diagnostics"
        guard isInputEvent || event == "operation_complete" || event == "error",
              let requestID, let request = steamInputRequests[requestID] else { return isInputEvent }
        guard account?.id == request.accountID else {
            steamInputRequests.removeValue(forKey: requestID)
            requestCommands.removeValue(forKey: requestID)
            return true
        }
        let appID = request.appID
        if isInputEvent {
            guard !request.invalidated, request.environment == steamInputEnvironment(for: appID) else {
                steamInputReports[appID] = nil
                steamInputErrors[appID] = "Настройки изменились. Требуется новая проверка Steam Input."
                return true
            }
            let owner = payload["steamid"] as? String
            let state = payload["state"] as? String ?? ""
            let preAuthenticationStatus = event == "steam_input_status" && owner == nil
                && ["authenticating", "failed"].contains(state)
            guard payload["appid"] as? Int == appID,
                  owner == request.accountID || preAuthenticationStatus else {
                steamInputErrors[appID] = "Ответ проверки не соответствует выбранной игре или аккаунту. Повторите проверку."
                return true
            }
            if event == "steam_input_diagnostics" {
                guard let report = SteamInputReport(json: payload) else {
                    steamInputErrors[appID] = "Получен неполный отчёт Steam Input. Повторите проверку."
                    return true
                }
                steamInputReports[appID] = report
                steamInputMessages[appID] = "Завершаем проверку…"
            } else {
                let state = payload["state"] as? String ?? ""
                if state == "failed" {
                    steamInputErrors[appID] = payload["message"] as? String ?? "Проверка Steam Input завершилась с ошибкой."
                } else {
                    let labels = ["authenticating": "Подключаем аккаунт…", "authenticated": "Аккаунт подключён", "starting": "Сканируем контроллеры, около 7 секунд…", "exited": "Завершаем проверку…"]
                    steamInputMessages[appID] = labels[state] ?? payload["message"] as? String ?? "Проверяем Steam Input…"
                }
            }
        } else {
            steamInputRequests.removeValue(forKey: requestID)
            requestCommands.removeValue(forKey: requestID)
            steamInputMessages[appID] = nil
            if event == "error" {
                steamInputErrors[appID] = payload["message"] as? String ?? "Не удалось проверить Steam Input. Откройте журнал для подробностей."
            } else if steamInputReports[appID] == nil && steamInputErrors[appID] == nil {
                steamInputErrors[appID] = "Проверка завершилась без отчёта. Повторите попытку или откройте журнал."
            }
        }
        return true
    }
}

struct SteamInputDiagnosticCard: View {
    @ObservedObject var model: AppModel
    let game: SteamGame
    var hasUnsavedChanges = false
    @State private var showLog = false
    private var running: Bool { model.steamInputRequests.values.contains { $0.appID == game.id } }
    private var report: SteamInputReport? { hasUnsavedChanges ? nil : model.steamInputReports[game.id] }
    private var blocked: String? {
        if hasUnsavedChanges { return "Сохраните изменения перед проверкой." }
        return model.steamInputBlockReason(for: game)
    }

    var body: some View {
        SettingsCard(title: "Steam Input", symbol: "gamecontroller") {
            VStack(alignment: .leading, spacing: 14) {
                Text("Подключите один контроллер перед проверкой. Во время сканирования нажимайте кнопки и двигайте стики. Проверка временно включает Steam Input, не меняя настройку игры.")
                    .font(.system(size: 11)).foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 12) {
                    Button { model.diagnoseSteamInput(for: game) } label: {
                        HStack(spacing: 7) {
                            if running { ProgressView().controlSize(.small) }
                            Text(running ? "Проверка…" : "Проверить Steam Input")
                        }.font(.system(size: 11))
                    }.disabled(blocked != nil)
                    Button("Журнал") { model.loadLaunchLog(for: game); showLog = true }
                        .buttonStyle(.plain).font(.system(size: 11)).foregroundStyle(Theme.muted).disabled(!model.coreReady)
                    Spacer()
                }
                if running, let message = model.steamInputMessages[game.id] {
                    Text(message).font(.system(size: 11)).foregroundStyle(Theme.muted)
                } else if let blocked {
                    Text(blocked).font(.system(size: 10)).foregroundStyle(Theme.muted)
                }
                if !hasUnsavedChanges, let error = model.steamInputErrors[game.id] {
                    Label(error, systemImage: "exclamationmark.circle").font(.system(size: 11))
                        .foregroundStyle(.orange).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                }
                if let report {
                    Divider().overlay(Theme.divider)
                    if report.fixture {
                        Label("Тестовый отчёт: физическое устройство не проверялось", systemImage: "info.circle")
                            .font(.system(size: 11)).foregroundStyle(.orange)
                    }
                    diagnosticRow("Steam Input API", result: report.apiInitialized ? "Инициализирован" : "Не инициализирован")
                    diagnosticRow("Аккаунт", result: report.identityVerified ? "Подтверждён" : "Не подтверждён")
                    diagnosticRow("Сканирование", result: report.scanCompleted ? "Завершено" : "Не завершено")
                    Text(report.controllerDescription).font(.system(size: 12, weight: .medium))
                        .foregroundStyle(Theme.muted)
                    ForEach(report.devices) { device in
                        VStack(alignment: .leading, spacing: 5) {
                            Text("Контроллер \(device.number)").font(.system(size: 11, weight: .medium))
                            Text(device.mappingDescription).font(.system(size: 10)).foregroundStyle(Theme.muted)
                            Text(device.bindingRevisionDescription)
                                .font(.system(size: 10)).foregroundStyle(Theme.muted)
                        }
                    }
                    diagnosticRow("Маршрут XInput", result: report.xinputRouteObserved ? "Обнаружен" : "Не обнаружен")
                    diagnosticRow("Движения и нажатия", result: report.activityVerified ? "Зафиксированы" : "Не зафиксированы")
                    Text(report.physicalDeviceVerified ? "Физическое устройство подтверждено отчётом." : "Работа физического контроллера не подтверждена.")
                        .font(.system(size: 10)).foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
                }
            }
        }.sheet(isPresented: $showLog) { LaunchLogView(model: model, game: game) }
    }

    private func diagnosticRow(_ title: String, result: String) -> some View {
        HStack {
            Text(title).foregroundStyle(Theme.muted)
            Spacer()
            Text(result)
        }.font(.system(size: 11))
    }
}
