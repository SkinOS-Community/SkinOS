import SwiftUI

struct UninstallPlan: Equatable {
    let appID: Int
    let requestID: String
    let installDirectory: String
    let additionalInstallDirectories: [String]
    let partialDirectories: [String]
    let sharedAppIDs: [Int]
    let removeBytes: Int64
    let confirmationToken: String

    init?(json: [String: Any], requestID: String) {
        guard let appID = json["appid"] as? Int, let directory = json["install_dir"] as? String,
              let token = json["confirmation_token"] as? String, !token.isEmpty,
              let bytes = (json["remove_bytes"] as? NSNumber)?.int64Value, bytes >= 0 else { return nil }
        self.appID = appID; self.requestID = requestID; installDirectory = directory
        confirmationToken = token; removeBytes = bytes
        additionalInstallDirectories = json["additional_install_dirs"] as? [String] ?? []
        partialDirectories = json["partial_dirs"] as? [String] ?? []
        sharedAppIDs = json["shared_appids"] as? [Int] ?? []
    }

    var confirmationText: String {
        var paragraphs: [String] = []
        if sharedAppIDs.isEmpty {
            paragraphs.append("Будут безвозвратно удалены файлы игры, включая моды и сохранения внутри её папок:\n" + ([installDirectory] + additionalInstallDirectories).joined(separator: "\n"))
        } else {
            paragraphs.append("Общая папка нужна другим играм и останется. Будет удалена запись установки этой игры и её незавершённая загрузка.")
            paragraphs.append("Общая папка останется:\n\(installDirectory)")
            if !additionalInstallDirectories.isEmpty {
                paragraphs.append("Также будут безвозвратно удалены старые копии игры, включая файлы внутри:\n" + additionalInstallDirectories.joined(separator: "\n"))
            }
        }
        if !partialDirectories.isEmpty { paragraphs.append("Незавершённая загрузка будет удалена:\n" + partialDirectories.joined(separator: "\n")) }
        paragraphs.append("Объём удаляемых файлов: \(GameDownload.bytes(removeBytes))")
        paragraphs.append("Сохранения вне удаляемых папок и среды CrossOver останутся. Игру можно установить снова из библиотеки.")
        return paragraphs.joined(separator: "\n\n")
    }
}

@MainActor
extension AppModel {
    func canUninstall(_ game: SteamGame) -> Bool {
        game.isInstalled && games.contains(where: { $0.id == game.id && $0.installedDirectory == game.installedDirectory })
        && account != nil && coreReady && !launchIsActive(game)
        && downloads[game.id]?.isActive != true && !pendingEnqueueGames.contains(game.id)
        && !pendingDownloadControls.contains(game.id) && !uninstallingGames.contains(game.id)
        && !loadingUninstallPlans.contains(game.id)
    }

    @discardableResult
    func requestUninstallPlan(for game: SteamGame) -> String? {
        guard canUninstall(game) else { return nil }
        errorMessage = nil; uninstallNotices[game.id] = nil
        guard let id = send("get_uninstall_plan", payload: ["appid": game.id]) else { return nil }
        uninstallPlans.removeValue(forKey: game.id)
        uninstallPlanRequests[id] = game.id
        loadingUninstallPlans.insert(game.id)
        return id
    }

    func uninstall(_ game: SteamGame, plan: UninstallPlan) {
        guard canUninstall(game), plan.appID == game.id, uninstallPlans[game.id] == plan else { return }
        errorMessage = nil
        uninstallNotices[game.id] = nil
        if let id = send("uninstall_game", payload: ["appid": game.id, "confirmation_token": plan.confirmationToken]) {
            uninstallRequests[id] = game.id
            uninstallingGames.insert(game.id)
            uninstallPlans.removeValue(forKey: game.id)
        }
    }

    func receiveUninstall(_ event: String, payload: [String: Any], requestID: String?) -> Bool {
        guard let requestID else { return event == "game_uninstalled" || event == "uninstall_plan" }
        if event == "uninstall_plan" || (event == "error" && uninstallPlanRequests[requestID] != nil) {
            guard let appID = uninstallPlanRequests.removeValue(forKey: requestID) else { return true }
            loadingUninstallPlans.remove(appID)
            if event == "error" { errorMessage = payload["message"] as? String ?? "Не удалось подготовить удаление игры." }
            else if let plan = UninstallPlan(json: payload, requestID: requestID), plan.appID == appID {
                uninstallPlans[appID] = plan
            } else { errorMessage = "Не удалось проверить список удаляемых файлов. Повторите подготовку удаления." }
            return true
        }
        guard ["game_uninstalled", "operation_complete", "error"].contains(event),
              let appID = uninstallRequests[requestID] else { return event == "game_uninstalled" }
        if event == "game_uninstalled" {
            guard payload["appid"] as? Int == appID else {
                errorMessage = "Не удалось подтвердить удаление выбранной игры. Обновите библиотеку."
                uninstallingGames.remove(appID)
                uninstallRequests.removeValue(forKey: requestID)
                return true
            }
            if let index = games.firstIndex(where: { $0.id == appID }) {
                games[index].installedDirectory = nil
                games[index].installedBuildID = nil
                games[index].installedSizeBytes = nil
                games[index].installedPlatform = nil
                games[index].updateAvailable = false
            }
            launchPhases.removeValue(forKey: appID)
            launchStates.removeValue(forKey: appID)
            uninstallNotices[appID] = payload["shared_directory_retained"] as? Bool == true
                ? "Игра удалена из установленных. Общая папка сохранена для других игр."
                : "Игра удалена с устройства"
        } else {
            uninstallingGames.remove(appID)
            uninstallRequests.removeValue(forKey: requestID)
            if event == "error" { errorMessage = payload["message"] as? String ?? "Не удалось удалить игру с устройства." }
        }
        return true
    }
}

struct GameUninstallButton: View {
    @ObservedObject var model: AppModel
    let game: SteamGame
    @State private var showingConfirmation = false
    @State private var requestedPlanID: String?
    @State private var confirmationPlan: UninstallPlan?
    private var currentGame: SteamGame { model.games.first { $0.id == game.id } ?? game }
    private var deleting: Bool { model.uninstallingGames.contains(game.id) }
    private var preparing: Bool { model.loadingUninstallPlans.contains(game.id) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if currentGame.isInstalled {
                Button(role: .destructive) { requestedPlanID = model.requestUninstallPlan(for: currentGame) } label: {
                    HStack(spacing: 7) {
                        if deleting || preparing { ProgressView().controlSize(.mini) }
                        Label(deleting ? "Удаляем с устройства…" : preparing ? "Подготовка удаления…" : "Удалить с устройства", systemImage: "trash")
                    }.font(.system(size: 11))
                }.buttonStyle(.bordered).disabled(!model.canUninstall(currentGame))
                    .help(model.launchIsActive(currentGame) ? "Сначала завершите игру" : model.downloads[game.id]?.isActive == true ? "Сначала уберите загрузку из очереди" : "Удалить установленные файлы игры")
                    .alert("Удалить «\(currentGame.name)» с устройства?", isPresented: $showingConfirmation) {
                        Button("Отмена", role: .cancel) { confirmationPlan = nil }
                        Button("Удалить безвозвратно", role: .destructive) {
                            if let plan = confirmationPlan { model.uninstall(currentGame, plan: plan) }
                            confirmationPlan = nil
                        }
                    } message: {
                        Text(confirmationPlan?.confirmationText ?? "Не удалось получить список файлов.")
                    }
            }
            if currentGame.isInstalled, model.downloads[game.id]?.isActive == true {
                Text("Сначала уберите загрузку из очереди.").font(.system(size: 10)).foregroundStyle(Theme.muted)
            }
            if let notice = model.uninstallNotices[game.id] {
                Text(notice).font(.system(size: 10)).foregroundStyle(Theme.muted)
            }
        }.onChange(of: model.uninstallPlans[game.id]?.requestID) { _, id in
            guard let id, id == requestedPlanID, let plan = model.uninstallPlans[game.id] else { return }
            requestedPlanID = nil
            confirmationPlan = plan
            showingConfirmation = true
        }
    }
}
