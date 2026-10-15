import SwiftUI

enum InstallPlatform: String, CaseIterable, Identifiable {
    case macOS = "macos"
    case windows

    var id: Self { self }
    var title: String { self == .macOS ? "macOS" : "Windows" }
    var symbol: String { self == .macOS ? "apple.logo" : "desktopcomputer" }
    var downloadLabel: String { self == .macOS ? "Версия для macOS" : "Windows · NotProton" }

    func isSupported(by game: SteamGame) -> Bool {
        self == .macOS ? game.supportsMacOS : game.supportsWindows
    }
}

extension SteamGame {
    var availableInstallPlatforms: [InstallPlatform] { InstallPlatform.allCases.filter { $0.isSupported(by: self) } }
    var installedVersion: InstallPlatform? {
        guard isInstalled, let installedPlatform else { return nil }
        return InstallPlatform(rawValue: installedPlatform)
    }
}

struct GameInstallationRequest: Identifiable {
    let id = UUID()
    let game: SteamGame
    let accountID: String
    let defaultPlatform: InstallPlatform

    init?(game: SteamGame, accountID: String) {
        // Download selection is independent of saved launch/compatibility settings.
        guard let platform = game.availableInstallPlatforms.first else { return nil }
        self.game = game
        self.accountID = accountID
        defaultPlatform = platform
    }
}

@MainActor
extension AppModel {
    func install(_ game: SteamGame, update: Bool = false) {
        guard let account else { isShowingLogin = true; return }
        guard coreReady else { errorMessage = "Нет соединения с SkinOS Core. Перезапустите приложение."; return }
        guard !pendingEnqueueGames.contains(game.id), !uninstallingGames.contains(game.id),
              downloads[game.id]?.isActive != true, !launchIsActive(game) else { return }
        let currentGame = games.first { $0.id == game.id } ?? game
        errorMessage = nil
        if update {
            guard currentGame.isInstalled else { return }
            // The backend resolves the installed platform. Never turn an update
            // into a conversion based on platform support or launch preferences.
            _ = enqueueInstallation(currentGame, update: true, platform: currentGame.installedVersion)
        } else {
            guard !currentGame.isInstalled else { return }
            installationRequest = GameInstallationRequest(game: currentGame, accountID: account.id)
            if installationRequest == nil { errorMessage = "У этой игры нет версии для macOS или Windows." }
        }
    }

    func cancelInstallation() { installationRequest = nil }

    @discardableResult
    func confirmInstallation(_ platform: InstallPlatform, requestID: UUID) -> Bool {
        guard let request = installationRequest, request.id == requestID,
              account?.id == request.accountID, coreReady else { return false }
        let game = games.first { $0.id == request.game.id } ?? request.game
        guard platform.isSupported(by: game), !game.isInstalled,
              !pendingEnqueueGames.contains(game.id), !uninstallingGames.contains(game.id),
              downloads[game.id]?.isActive != true, !launchIsActive(game) else { return false }
        guard enqueueInstallation(game, update: false, platform: platform) else { return false }
        installationRequest = nil
        return true
    }

    private func enqueueInstallation(_ game: SteamGame, update: Bool, platform: InstallPlatform?) -> Bool {
        var payload: [String: Any] = ["appid": game.id]
        if !update {
            guard let platform else { return false }
            payload["platform"] = platform.rawValue
        }
        errorMessage = nil
        guard let id = send(update ? "update_game" : "install_game", payload: payload) else { return false }
        pendingEnqueueGames.insert(game.id)
        gameRequests[id] = game.id
        if !downloadQueueReady {
            downloads[game.id] = GameDownload(id: game.id, phase: "queued", isUpdate: update, platform: platform)
            if !downloadOrder.contains(game.id) { downloadOrder.append(game.id) }
        }
        return true
    }
}

struct InstallPlatformView: View {
    @ObservedObject var model: AppModel
    let request: GameInstallationRequest
    @State private var selectedPlatform: InstallPlatform

    init(model: AppModel, request: GameInstallationRequest) {
        self.model = model
        self.request = request
        _selectedPlatform = State(initialValue: request.defaultPlatform)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 16) {
                GameArtwork(url: request.game.headerURL).frame(width: 112, height: 53)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                VStack(alignment: .leading, spacing: 6) {
                    Text("УСТАНОВКА ИГРЫ").font(.system(size: 9, weight: .semibold)).tracking(1.4).foregroundStyle(Theme.muted)
                    Text(request.game.name).font(.system(size: 22, weight: .bold, design: .rounded)).lineLimit(2)
                }
                Spacer(minLength: 0)
                Button { model.cancelInstallation() } label: { Image(systemName: "xmark").font(.system(size: 12)) }
                    .buttonStyle(.plain).foregroundStyle(Theme.muted).accessibilityLabel("Отменить установку")
            }.padding(25)
            Divider().overlay(Theme.divider)
            VStack(alignment: .leading, spacing: 13) {
                Text("Какую версию скачать?").font(.system(size: 16, weight: .semibold))
                ForEach(InstallPlatform.allCases) { platform in
                    platformOption(platform)
                }
                if selectedPlatform == .windows {
                    Text("Для запуска Windows-версии нужен установленный CrossOver или CrossOver Preview. NotProton уже встроен в SkinOS.")
                        .font(.system(size: 11)).foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
                }
                Text("Папка установки: \(model.settings.steamappsDirectory)")
                    .font(.system(size: 10)).foregroundStyle(Theme.muted).lineLimit(2).textSelection(.enabled)
                if let error = model.errorMessage {
                    Text(error).font(.system(size: 11)).foregroundStyle(.orange).fixedSize(horizontal: false, vertical: true)
                }
            }.padding(25)
            Divider().overlay(Theme.divider)
            HStack(spacing: 13) {
                Spacer()
                Button("Отмена") { model.cancelInstallation() }.keyboardShortcut(.cancelAction)
                Button { model.confirmInstallation(selectedPlatform, requestID: request.id) } label: {
                    Label("Скачать для \(selectedPlatform.title)", systemImage: "arrow.down")
                        .font(.system(size: 12, weight: .semibold)).padding(.horizontal, 14).padding(.vertical, 9)
                }.buttonStyle(AccentButtonStyle()).keyboardShortcut(.defaultAction)
                    .disabled(!model.coreReady || model.account?.id != request.accountID || !selectedPlatform.isSupported(by: request.game))
            }.padding(20)
        }.frame(width: 540).background(Theme.background).preferredColorScheme(.dark)
    }

    private func platformOption(_ platform: InstallPlatform) -> some View {
        let supported = platform.isSupported(by: request.game)
        let selected = selectedPlatform == platform
        return Button { selectedPlatform = platform } label: {
            HStack(spacing: 14) {
                Image(systemName: platform.symbol).font(.system(size: 20)).frame(width: 28)
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 8) {
                        Text(platform.title).font(.system(size: 14, weight: .semibold))
                        if platform == .macOS, supported {
                            Text("РЕКОМЕНДУЕТСЯ").font(.system(size: 8, weight: .semibold)).tracking(0.6).foregroundStyle(Theme.accent)
                        }
                    }
                    Text(!supported ? "Нет версии для этой платформы" : platform == .macOS ? "Версия для Mac · без CrossOver" : "Запуск через NotProton и CrossOver")
                        .font(.system(size: 11)).foregroundStyle(Theme.muted)
                }
                Spacer()
                Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 20)).foregroundStyle(selected ? Theme.accent : Theme.muted)
            }.foregroundStyle(supported ? Color.white : Theme.muted).padding(17).frame(maxWidth: .infinity, alignment: .leading)
                .background(selected ? Theme.accent.opacity(0.06) : Theme.surface, in: RoundedRectangle(cornerRadius: 11))
                .overlay(RoundedRectangle(cornerRadius: 11).stroke(selected ? Theme.accent.opacity(0.7) : Theme.divider, lineWidth: 1))
                .opacity(supported ? 1 : 0.45).contentShape(RoundedRectangle(cornerRadius: 11))
        }.buttonStyle(.plain).disabled(!supported)
            .accessibilityLabel("\(platform.title), \(supported ? selected ? "выбрана" : "доступна" : "недоступна")")
            .accessibilityAddTraits(selected ? .isSelected : [])
    }
}
