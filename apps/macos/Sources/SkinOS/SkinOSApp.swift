import SwiftUI
import AppKit
import CoreImage.CIFilterBuiltins

enum Theme {
    static let background = Color(red: 0.060, green: 0.065, blue: 0.080)
    static let sidebar = Color(red: 0.043, green: 0.047, blue: 0.058)
    static let surface = Color(red: 0.094, green: 0.100, blue: 0.122)
    static let elevated = Color(red: 0.125, green: 0.135, blue: 0.155)
    static let accent = Color(red: 0.76, green: 0.92, blue: 0.53)
    static let muted = Color(red: 0.52, green: 0.56, blue: 0.61)
    static let divider = Color.white.opacity(0.065)
}

@main
struct SkinOSApp: App {
    @NSApplicationDelegateAdaptor(SkinOSApplicationDelegate.self) private var appDelegate
    @StateObject private var model = AppModel(notifications: ClientNotifications.shared)
    @StateObject private var updates = AppUpdates()

    var body: some Scene {
        WindowGroup(id: "main") {
            RootView(model: model)
                .environmentObject(updates)
                .frame(minWidth: 960, minHeight: 660)
                .preferredColorScheme(.dark)
                .tint(Theme.accent)
                .onAppear {
                    model.start()
                    appDelegate.updates = updates
                    updates.start(model: model)
                    NSApplication.shared.activate(ignoringOtherApps: true)
                }
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in model.updateApplicationActivity(true) }
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.didResignActiveNotification)) { _ in model.updateApplicationActivity(false) }
                .onReceive(NotificationCenter.default.publisher(for: NSWindow.didBecomeKeyNotification)) { _ in model.updateWindowVisibility() }
                .onReceive(NotificationCenter.default.publisher(for: NSWindow.didResignKeyNotification)) { _ in model.updateWindowVisibility() }
                .onReceive(NotificationCenter.default.publisher(for: NSWindow.didMiniaturizeNotification)) { _ in model.updateWindowVisibility() }
                .onReceive(NotificationCenter.default.publisher(for: NSWindow.didDeminiaturizeNotification)) { _ in model.updateWindowVisibility() }
                .onReceive(NotificationCenter.default.publisher(for: NSWindow.willCloseNotification)) { notification in
                    if let window = notification.object as? NSWindow, window === model.mainApplicationWindow {
                        model.applicationWindowVisible = false
                        model.mainApplicationWindow = nil
                    }
                }
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in model.shutdown() }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1280, height: 820)
        .commands {
            CommandGroup(after: .appInfo) {
                Button("Проверить обновления SkinOS…") { updates.checkForUpdates() }
                    .disabled(!updates.canCheckForUpdates)
            }
            CommandGroup(replacing: .appSettings) {
                Button("Настройки…") { model.section = .settings }
                    .keyboardShortcut(",", modifiers: .command)
            }
            CommandGroup(replacing: .newItem) {
                Button("Обновить библиотеку") { model.refreshLibrary() }
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(model.account == nil)
            }
        }
    }
}

struct RootView: View {
    @ObservedObject var model: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        HStack(spacing: 0) {
            SidebarView(model: model)
            Rectangle().fill(Theme.divider).frame(width: 1)
            VStack(spacing: 0) {
                TopBar(model: model)
                if model.isReconnecting || model.needsFreshLogin || (model.isAuthenticating && !model.isShowingLogin) {
                    HStack(spacing: 12) {
                        if model.needsFreshLogin { Image(systemName: "person.crop.circle.badge.exclamationmark").foregroundStyle(.orange) }
                        else { ProgressView().controlSize(.small) }
                        Text(model.needsFreshLogin ? model.authMessage : model.reconnectSeconds.map { $0 > 0 ? "Соединение прервано. Повтор через \($0) с" : "Восстанавливаем подключение…" } ?? model.authMessage)
                            .font(.system(size: 12)).monospacedDigit()
                        Spacer()
                        if model.needsFreshLogin {
                            Button("Войти через QR-код") { model.startQRLogin() }
                        } else if model.isReconnecting, model.reconnectSeconds != nil {
                            Button("Повторить сейчас") { model.retryConnection() }
                        }
                        if !model.needsFreshLogin { Button("Отменить") { model.cancelAuth() } }
                    }.padding(14).background(Theme.accent.opacity(0.06))
                }
                if let error = model.errorMessage {
                    HStack(alignment: .top, spacing: 10) {
                        Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.orange)
                        Text(error).font(.system(size: 12)).textSelection(.enabled)
                        Spacer(minLength: 10)
                        if let game = model.selectedGame, model.launchPhases[game.id] == "failed" {
                            Button("Журнал") { model.showLaunchLog(for: game) }.disabled(!model.coreReady)
                        }
                        Button { model.errorMessage = nil } label: { Image(systemName: "xmark") }
                            .buttonStyle(.plain).accessibilityLabel("Закрыть сообщение")
                    }
                    .padding(14).background(Color.orange.opacity(0.08))
                }
                switch model.section {
                case .library: LibraryView(model: model)
                case .store: SteamBrowserView(model: model, target: "store").id("store-\(model.webIdentity)")
                case .profile: SteamBrowserView(model: model, target: "community").id("profile-\(model.webIdentity)")
                case .friends: FriendsView(model: model)
                case .notifications: NotificationsView(model: model)
                case .downloads: DownloadsView(model: model)
                case .account: AccountView(model: model)
                case .settings: SettingsView(model: model)
                }
            }
        }
        .background(Theme.background)
        .background(WindowAccess { window in
            if let window { model.mainApplicationWindow = window }
        })
        .onAppear {
            model.activateApplication = {
                if let window = model.mainApplicationWindow {
                    if window.isMiniaturized { window.deminiaturize(nil) }
                    window.makeKeyAndOrderFront(nil)
                } else { openWindow(id: "main") }
                NSApplication.shared.activate(ignoringOtherApps: true)
            }
        }
        .sheet(isPresented: $model.isShowingLogin, onDismiss: { model.cancelAuth() }) {
            LoginView(model: model)
        }
        .sheet(item: $model.gameForSettings) { game in
            GameSettingsView(model: model, game: game)
        }
        .sheet(item: $model.gameForLog) { game in
            LaunchLogView(model: model, game: game)
        }
        .sheet(item: $model.installationRequest) { request in
            InstallPlatformView(model: model, request: request)
        }
    }
}

struct SidebarView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 11) {
                ZStack {
                    RoundedRectangle(cornerRadius: 12).fill(Theme.accent).frame(width: 39, height: 39)
                    Image(systemName: "square.stack.3d.up.fill")
                        .font(.system(size: 19, weight: .bold)).foregroundStyle(Theme.sidebar)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text("SkinOS").font(.system(size: 22, weight: .bold, design: .rounded))
                    Text("STEAM ДЛЯ MAC").font(.system(size: 8, weight: .semibold)).tracking(1.25).foregroundStyle(Theme.muted)
                }
            }
            .padding(.top, 51).padding(.horizontal, 22).padding(.bottom, 43)

            Text("КОЛЛЕКЦИЯ").font(.system(size: 9, weight: .bold)).tracking(1.7)
                .foregroundStyle(Theme.muted).padding(.horizontal, 25).padding(.bottom, 14)
            ForEach(AppSection.allCases) { section in
                Button { model.section = section } label: {
                    HStack(spacing: 12) {
                        Image(systemName: model.section == section ? "checkmark" : section.symbol)
                            .font(.system(size: 15, weight: .medium)).frame(width: 20)
                            .accessibilityHidden(true)
                        Text(section.rawValue).font(.system(size: 13, weight: .medium)).lineLimit(1)
                        Spacer()
                        if section == .library, !model.games.isEmpty {
                            Text("\(model.games.count)").font(.system(size: 10, weight: .medium, design: .monospaced))
                        } else if section == .notifications, model.unreadNoticeCount > 0 {
                            Text("\(model.unreadNoticeCount)").font(.system(size: 10, weight: .bold))
                        } else if section == .friends, model.unreadTotal > 0 {
                            Text("\(model.unreadTotal)").font(.system(size: 10, weight: .bold))
                        } else if section == .downloads, model.activeDownloads > 0 {
                            Text("\(model.activeDownloads)").font(.system(size: 10, weight: .bold))
                        }
                    }
                    .foregroundStyle(model.section == section ? Theme.accent : Theme.muted)
                    .padding(.horizontal, 13).padding(.vertical, 13)
                    .background(model.section == section ? Theme.accent.opacity(0.085) : .clear, in: RoundedRectangle(cornerRadius: 9))
                    .contentShape(Rectangle())
                }.buttonStyle(.plain)
                    .accessibilityAddTraits(model.section == section ? .isSelected : [])
                    .padding(.horizontal, 12).padding(.bottom, 4)
            }

            Spacer(minLength: 30)
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 7) {
                    Circle().fill(model.coreReady ? Theme.accent : Theme.muted).frame(width: 5, height: 5)
                    Text(model.coreReady ? "КЛИЕНТ ГОТОВ" : "НЕТ СОЕДИНЕНИЯ")
                        .font(.system(size: 8, weight: .semibold, design: .monospaced)).tracking(1.1)
                }.foregroundStyle(Theme.muted)
                Text("Нативный клиент для Mac")
                    .font(.system(size: 10)).foregroundStyle(Theme.muted)
            }.padding(25)
            Rectangle().fill(Theme.divider).frame(height: 1)
            Button { model.account == nil ? (model.isShowingLogin = true) : (model.section = .account) } label: {
                HStack(spacing: 10) {
                    AvatarView(account: model.account, size: 32)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(model.account?.displayName ?? "Ваш аккаунт").font(.system(size: 12, weight: .semibold)).lineLimit(1)
                        Text(model.account == nil ? "Войти через Steam" : !model.steamConnected ? "Подключение…" : model.presence.title)
                            .font(.system(size: 10)).foregroundStyle(Theme.muted)
                    }
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.up.chevron.down").font(.system(size: 9)).foregroundStyle(Theme.muted)
                }.padding(18).contentShape(Rectangle())
            }.buttonStyle(.plain)
        }
        .frame(width: 214).background(Theme.sidebar)
    }
}

struct TopBar: View {
    @ObservedObject var model: AppModel
    var body: some View {
        HStack(spacing: 14) {
            Text(model.section.rawValue).font(.system(size: 12, weight: .medium)).foregroundStyle(Theme.muted)
            Spacer()
            if model.isRefreshing { ProgressView().controlSize(.small) }
            if model.section == .library {
                HStack(spacing: 9) {
                    Image(systemName: "magnifyingglass").font(.system(size: 12)).foregroundStyle(Theme.muted)
                    TextField("Найти игру", text: $model.search).textFieldStyle(.plain).font(.system(size: 12))
                    if !model.search.isEmpty {
                        Button { model.search = "" } label: { Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.muted) }.buttonStyle(.plain)
                    }
                }.padding(.horizontal, 12).frame(width: 240, height: 33)
                    .background(Theme.surface, in: RoundedRectangle(cornerRadius: 8))
                Button { model.refreshLibrary() } label: { Image(systemName: "arrow.clockwise").font(.system(size: 13)) }
                    .buttonStyle(.plain).foregroundStyle(Theme.muted)
                    .disabled(model.account == nil || model.isRefreshing).help("Обновить библиотеку · ⌘R")
            }
        }.padding(.horizontal, 30).frame(height: 72)
        Rectangle().fill(Theme.divider).frame(height: 1)
    }
}

struct LibraryView: View {
    @ObservedObject var model: AppModel
    private let columns = [GridItem(.adaptive(minimum: 205, maximum: 330), spacing: 20)]

    var body: some View {
        HStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 0) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 9) {
                        Text("Библиотека")
                            .font(.system(size: 27, weight: .bold, design: .rounded)).tracking(-0.8)
                        Text(subtitle).font(.system(size: 12)).foregroundStyle(Theme.muted)
                    }
                    Spacer()
                }.padding(.bottom, 28)
                LibraryFilters(model: model)

                if model.account == nil {
                    WelcomeView(model: model)
                } else if model.games.isEmpty && model.isRefreshing {
                    VStack(spacing: 16) { ProgressView(); Text("Собираем вашу библиотеку…").font(.system(size: 13)).foregroundStyle(Theme.muted) }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if model.filteredGames.isEmpty {
                    EmptyState(symbol: "square.grid.2x2", title: "Здесь пока нет игр",
                        subtitle: model.search.isEmpty ? "Попробуйте другой фильтр или обновите библиотеку." : "По запросу «\(model.search)» ничего не найдено.")
                } else {
                    ScrollView {
                        LazyVGrid(columns: columns, spacing: 25) {
                            ForEach(model.filteredGames) { game in
                                GameCard(game: game, selected: model.selectedGameID == game.id, download: model.downloads[game.id]) {
                                    withAnimation(.easeInOut(duration: 0.18)) { model.selectedGameID = game.id }
                                    model.loadDetails(game)
                                }.contextMenu {
                                    Button(model.favoriteIDs.contains(game.id) ? "Убрать из избранного" : "В избранное") { model.toggleFavorite(game) }
                                    Button("Открыть в магазине") { model.openStore(game) }
                                }
                            }
                        }.padding(.bottom, 30)
                    }.scrollIndicators(.hidden)
                }
            }.padding(.horizontal, 30).padding(.top, 32)
            if let game = model.selectedGame {
                Rectangle().fill(Theme.divider).frame(width: 1)
                GameDetailView(model: model, game: game).frame(width: 282)
            }
        }
    }

    var subtitle: String {
        guard model.account != nil else { return "Подключите аккаунт Steam, чтобы загрузить список игр." }
        return model.libraryIsCached ? "Сохранённая библиотека · обновите, чтобы получить последние данные." : "Игры текущего аккаунта Steam."
    }
}

struct WelcomeView: View {
    @ObservedObject var model: AppModel
    var body: some View {
        VStack(spacing: 0) {
            Spacer()
            ZStack {
                Circle().fill(Theme.accent.opacity(0.025)).frame(width: 196, height: 196)
                Circle().stroke(Theme.accent.opacity(0.1), lineWidth: 1).frame(width: 154, height: 154)
                RoundedRectangle(cornerRadius: 22).fill(Theme.surface).frame(width: 90, height: 90).rotationEffect(.degrees(-8))
                Image(systemName: "gamecontroller.fill").font(.system(size: 35)).foregroundStyle(Theme.accent)
            }.padding(.bottom, 14)
            Text("Подключение Steam").font(.system(size: 24, weight: .semibold, design: .rounded)).tracking(-0.5)
            Text("Подключите Steam, чтобы увидеть свои игры,\nустанавливать их и запускать прямо отсюда.")
                .font(.system(size: 13)).foregroundStyle(Theme.muted).multilineTextAlignment(.center).lineSpacing(5).padding(.top, 13)
            if model.isAuthenticating || model.isReconnecting {
                ProgressView().controlSize(.small).padding(.top, 28)
                Text(model.authMessage).font(.system(size: 12)).foregroundStyle(Theme.muted).padding(.top, 12)
            } else if let saved = model.savedAccounts.first, !model.needsFreshLogin {
                Button { model.restore(saved) } label: {
                    Label("Продолжить как \(saved.displayName)", systemImage: "arrow.right")
                        .font(.system(size: 13, weight: .semibold)).padding(.horizontal, 25).padding(.vertical, 14)
                }.buttonStyle(AccentButtonStyle()).disabled(!model.coreReady || model.isAuthenticating).padding(.top, 28)
                Button("Войти в другой аккаунт") { model.startQRLogin() }
                    .buttonStyle(.plain).font(.system(size: 11)).foregroundStyle(Theme.accent).padding(.top, 13)
            } else {
                Button { model.startQRLogin() } label: {
                    Label("Подключить Steam", systemImage: "qrcode").font(.system(size: 13, weight: .semibold))
                        .padding(.horizontal, 25).padding(.vertical, 14)
                }.buttonStyle(AccentButtonStyle()).disabled(!model.coreReady).padding(.top, 28)
            }
            Text(model.isAuthenticating ? model.authMessage : "Вход через Steam Guard · пароль не нужен")
                .font(.system(size: 10)).foregroundStyle(Theme.muted).padding(.top, 16)
            Spacer()
            HStack(spacing: 28) {
                Label("Нативный интерфейс", systemImage: "apple.logo")
                Label("Ваша библиотека", systemImage: "square.stack")
                Label("Прямое подключение", systemImage: "bolt.horizontal")
            }.font(.system(size: 10)).foregroundStyle(Theme.muted).padding(.bottom, 33)
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct GameCard: View {
    let game: SteamGame
    let selected: Bool
    let download: GameDownload?
    let action: () -> Void
    @State private var hovered = false

    var body: some View {
        Button(action: action) {
            VStack(alignment: .leading, spacing: 12) {
                GameArtwork(url: game.headerURL)
                    .aspectRatio(460.0 / 215.0, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 11))
                    .overlay(RoundedRectangle(cornerRadius: 11).stroke(selected ? Theme.accent : .white.opacity(hovered ? 0.18 : 0.04), lineWidth: selected ? 2 : 1))
                    .overlay(alignment: .bottomLeading) {
                        if game.isInstalled {
                            Label(game.updateAvailable ? "Есть обновление" : game.installedVersion.map { "Установлена · \($0.title)" } ?? "Установлена", systemImage: game.updateAvailable ? "arrow.down.circle.fill" : "checkmark.circle.fill")
                                .font(.system(size: 8, weight: .semibold))
                                .padding(.horizontal, 7).padding(.vertical, 5).background(.black.opacity(0.65), in: Capsule()).padding(8)
                        }
                    }
                VStack(alignment: .leading, spacing: 6) {
                    Text(game.name).font(.system(size: 13, weight: .semibold)).lineLimit(1)
                    HStack {
                        Text(game.playtime).font(.system(size: 10)).foregroundStyle(Theme.muted)
                        Spacer()
                        if let platform = game.installedVersion {
                            Image(systemName: platform.symbol).font(.system(size: 10)).foregroundStyle(Theme.muted).help("Установлена версия \(platform.title)")
                        } else if game.supportsMacOS { Image(systemName: "apple.logo").font(.system(size: 10)).foregroundStyle(Theme.muted) }
                    }
                    if let download, download.isActive {
                        Text(download.phaseDescription + (download.platform.map { " · \($0.title)" } ?? "")).font(.system(size: 9, weight: .medium)).foregroundStyle(download.phase == "paused" ? Theme.muted : Theme.accent)
                        if let progress = download.displayProgress {
                            ProgressView(value: progress).tint(Theme.accent).controlSize(.small)
                        }
                    }
                }.padding(.horizontal, 2)
            }.contentShape(Rectangle())
        }.buttonStyle(.plain).onHover { hovered = $0 }
    }
}

struct GameDetailView: View {
    @ObservedObject var model: AppModel
    let game: SteamGame

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                HStack {
                    Text("ОБ ИГРЕ").font(.system(size: 9, weight: .bold)).tracking(1.5).foregroundStyle(Theme.muted)
                    Spacer()
                    Button { model.toggleFavorite(game) } label: { Image(systemName: model.favoriteIDs.contains(game.id) ? "star.fill" : "star").foregroundStyle(model.favoriteIDs.contains(game.id) ? Theme.accent : Theme.muted) }.buttonStyle(.plain).help("Избранное")
                    Button { withAnimation { model.selectedGameID = nil } } label: { Image(systemName: "xmark").font(.system(size: 11)).foregroundStyle(Theme.muted) }.buttonStyle(.plain)
                }
                GameArtwork(url: game.headerURL).aspectRatio(460.0 / 215.0, contentMode: .fit).clipShape(RoundedRectangle(cornerRadius: 10))
                VStack(alignment: .leading, spacing: 10) {
                    Text(game.name).font(.system(size: 23, weight: .bold, design: .rounded)).tracking(-0.6)
                    Label(platformLabel, systemImage: game.installedPlatform == "windows" || !game.supportsMacOS ? "desktopcomputer" : "apple.logo")
                        .font(.system(size: 10)).foregroundStyle(Theme.muted)
                }
                VStack(spacing: 11) {
                    if game.isInstalled {
                        Button { model.launch(game) } label: { Label(model.launchButtonTitle(game), systemImage: "play.fill").font(.system(size: 13, weight: .semibold)).frame(maxWidth: .infinity).padding(.vertical, 14) }
                            .buttonStyle(AccentButtonStyle())
                            .disabled(!model.steamConnected || !model.coreReady || model.launchIsActive(game) || downloadInProgress)
                        if ["running", "stopping"].contains(model.launchPhases[game.id] ?? "") {
                            Button { model.stopGame(game) } label: {
                                Label(model.launchPhases[game.id] == "stopping" ? "Завершаем…" : "Завершить", systemImage: "stop.fill")
                                    .font(.system(size: 13, weight: .semibold)).frame(maxWidth: .infinity).padding(.vertical, 11)
                            }.buttonStyle(.bordered).tint(.orange)
                                .disabled(model.launchPhases[game.id] != "running" || !model.coreReady)
                        }
                        if game.updateAvailable {
                            Button { model.install(game, update: true) } label: {
                                Label("Обновить игру", systemImage: "arrow.down.circle")
                                    .font(.system(size: 12, weight: .semibold)).frame(maxWidth: .infinity).padding(.vertical, 10)
                            }.buttonStyle(.bordered)
                                .disabled(downloadInProgress || model.launchIsActive(game) || !model.coreReady)
                        }
                    } else {
                        Button { model.install(game) } label: { Label("Установить", systemImage: "arrow.down").font(.system(size: 13, weight: .semibold)).frame(maxWidth: .infinity).padding(.vertical, 14) }
                            .buttonStyle(AccentButtonStyle())
                            .disabled(!model.canInstall(game) || downloadInProgress || !model.steamConnected || !model.coreReady)
                    }
                    if let state = model.launchStates[game.id] { Text(state).font(.system(size: 10)).foregroundStyle(Theme.muted).multilineTextAlignment(.center) }
                    if model.launchIsActive(game), let message = model.steamInputMessages[game.id] {
                        Label(message, systemImage: "gamecontroller")
                            .font(.system(size: 10)).foregroundStyle(Theme.muted)
                            .multilineTextAlignment(.center).fixedSize(horizontal: false, vertical: true)
                    }
                    if model.launchIsActive(game), let message = model.steamInputFeedbackErrors[game.id] {
                        Label(message, systemImage: "waveform")
                            .font(.system(size: 10)).foregroundStyle(.orange)
                            .multilineTextAlignment(.center).fixedSize(horizontal: false, vertical: true)
                    }
                    if game.isInstalled || model.launchPhases[game.id] != nil {
                        Button { model.showLaunchLog(for: game) } label: {
                            Label("Журнал запуска", systemImage: "doc.text.magnifyingglass")
                                .font(.system(size: 11)).frame(maxWidth: .infinity).padding(.vertical, 8)
                        }.buttonStyle(.bordered).disabled(!model.coreReady)
                    }
                    if let download = model.downloads[game.id], !download.complete {
                        DownloadMeter(download: download)
                        DownloadControls(model: model, download: download)
                        Button("Открыть загрузки") { model.section = .downloads }
                            .buttonStyle(.plain).font(.system(size: 10)).foregroundStyle(Theme.muted)
                    }
                    Button { model.openStore(game) } label: {
                        Label("Открыть в магазине", systemImage: "bag")
                            .font(.system(size: 11)).frame(maxWidth: .infinity).padding(.vertical, 8)
                    }.buttonStyle(.bordered)
                    Button { model.showSettings(for: game) } label: {
                        Label("Свойства игры", systemImage: "slider.horizontal.3")
                            .font(.system(size: 11)).frame(maxWidth: .infinity).padding(.vertical, 8)
                    }.buttonStyle(.bordered).disabled(!model.coreReady)
                    if game.isInstalled {
                        Button { model.checkUpdates(for: game) } label: {
                            HStack(spacing: 7) {
                                if model.isCheckingUpdates { ProgressView().controlSize(.mini) }
                                Text("Проверить обновления").font(.system(size: 10))
                            }
                        }.buttonStyle(.plain).foregroundStyle(Theme.muted)
                            .disabled(model.isCheckingUpdates || model.account == nil || !model.coreReady)
                        if let notice = model.updatesNotice {
                            Text(notice).font(.system(size: 10)).foregroundStyle(Theme.muted).multilineTextAlignment(.center)
                        }
                    }
                }
                Rectangle().fill(Theme.divider).frame(height: 1)
                VStack(alignment: .leading, spacing: 18) {
                    DetailValue(title: "АККАУНТ ДЛЯ ИГРЫ", value: model.account?.displayName ?? "не выбран", symbol: "person.crop.circle")
                    DetailValue(title: "ВРЕМЯ В ИГРЕ", value: game.playtime, symbol: "clock")
                    DetailValue(title: "СОСТОЯНИЕ", value: game.isInstalled ? "Установлена" : "В вашей библиотеке", symbol: game.isInstalled ? "internaldrive" : "checkmark.seal")
                }
                Rectangle().fill(Theme.divider).frame(height: 1)
                RequirementsView(model: model, game: game)
                GameUninstallButton(model: model, game: game)
                if let directory = game.installedDirectory {
                    Button { NSWorkspace.shared.open(URL(fileURLWithPath: directory, isDirectory: true)) } label: {
                        Label("Открыть папку игры", systemImage: "folder").font(.system(size: 11)).foregroundStyle(Theme.muted)
                    }.buttonStyle(.plain)
                }
            }.padding(23)
        }.background(Theme.sidebar.opacity(0.5))
    }

    private var platformLabel: String {
        if game.installedVersion == .macOS { return "Установлена версия для macOS" }
        if game.installedPlatform == "windows" { return "Windows · NotProton" }
        if game.supportsMacOS { return "Есть версия для macOS" }
        return game.supportsWindows ? "Windows · NotProton" : "Нет версии для Mac или Windows"
    }

    var downloadInProgress: Bool {
        if model.pendingEnqueueGames.contains(game.id) || model.uninstallingGames.contains(game.id) { return true }
        guard let download = model.downloads[game.id] else { return false }
        return download.isActive
    }
}


struct AccountView: View {
    @ObservedObject var model: AppModel
    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            Text("Аккаунты Steam").font(.system(size: 27, weight: .bold, design: .rounded)).tracking(-0.8)
            if let account = model.account {
                HStack(spacing: 20) {
                    AvatarView(account: account, size: 70)
                    VStack(alignment: .leading, spacing: 8) {
                        Text(account.displayName).font(.system(size: 23, weight: .semibold, design: .rounded))
                        Text(account.accountName).font(.system(size: 12)).foregroundStyle(Theme.muted)
                        Text(account.id).font(.system(size: 10, design: .monospaced)).foregroundStyle(Theme.muted).textSelection(.enabled)
                    }
                    Spacer()
                    PresenceMenu(model: model)
                }.padding(25).background(Theme.surface, in: RoundedRectangle(cornerRadius: 14))
                HStack(spacing: 35) {
                    DetailValue(title: "ИГР В БИБЛИОТЕКЕ", value: "\(model.games.count)", symbol: "square.stack")
                    DetailValue(title: "ДЛЯ MAC", value: "\(model.games.filter(\.supportsMacOS).count)", symbol: "apple.logo")
                    DetailValue(title: "УСТАНОВЛЕНО", value: "\(model.games.filter(\.isInstalled).count)", symbol: "internaldrive")
                }.padding(.vertical, 10)
                HStack(spacing: 15) {
                    Button("Открыть профиль Steam") { model.showProfile() }.buttonStyle(.bordered)
                    Button("Выйти из аккаунта") { model.logout() }.buttonStyle(.bordered)
                }
                Spacer()
            } else { WelcomeView(model: model) }
        }.padding(30).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

struct LoginView: View {
    @ObservedObject var model: AppModel
    @State private var mode = 0
    @State private var accountName = ""
    @State private var password = ""
    @State private var guardCode = ""

    var body: some View {
        VStack(spacing: 22) {
            HStack {
                Text("ПОДКЛЮЧЕНИЕ STEAM").font(.system(size: 9, weight: .semibold)).tracking(1.6).foregroundStyle(Theme.muted)
                Spacer()
                Button { password = ""; guardCode = ""; model.cancelAuth() } label: { Image(systemName: "xmark").font(.system(size: 12)) }.buttonStyle(.plain)
            }
            VStack(spacing: 10) {
                Text("Вход в Steam").font(.system(size: 27, weight: .bold, design: .rounded)).tracking(-0.7)
                Text("Войдите в аккаунт, которому принадлежат игры.").font(.system(size: 12)).foregroundStyle(Theme.muted)
            }
            Picker("Способ входа", selection: $mode) {
                Text("QR-код").tag(0)
                Text("Логин и пароль").tag(1)
            }.pickerStyle(.segmented).frame(width: 300)
                .onChange(of: mode) { _, newValue in
                    model.cancelAuth(dismiss: false)
                    password = ""; guardCode = ""
                    if newValue == 0 { model.startQRLogin() }
                }
            if model.guardRequired {
                VStack(spacing: 16) {
                    Image(systemName: "lock.shield").font(.system(size: 36)).foregroundStyle(Theme.accent)
                    Text(model.guardMessage).font(.system(size: 12)).multilineTextAlignment(.center)
                    SecureField("Код Steam Guard", text: $guardCode).textFieldStyle(.roundedBorder).frame(width: 245)
                        .onSubmit { submitGuard() }
                    Button("Подтвердить", action: submitGuard).buttonStyle(.borderedProminent).disabled(guardCode.isEmpty)
                }.frame(height: 235)
            } else if mode == 0 {
                ZStack {
                    RoundedRectangle(cornerRadius: 17).fill(Color.white).frame(width: 220, height: 220)
                    if let url = model.qrURL, let image = qrImage(url) {
                        Image(nsImage: image).interpolation(.none).resizable().frame(width: 192, height: 192)
                    } else {
                        VStack(spacing: 15) {
                            if model.isAuthenticating { ProgressView().tint(.black) }
                            else { Image(systemName: "qrcode").font(.system(size: 48)).foregroundStyle(.black.opacity(0.65)) }
                            Text(model.isAuthenticating ? "Создаём QR-код…" : "QR-код не готов").font(.system(size: 11)).foregroundStyle(.black.opacity(0.6))
                        }
                    }
                }.frame(height: 235)
            } else {
                VStack(alignment: .leading, spacing: 15) {
                    Text("ЛОГИН STEAM").font(.system(size: 9, weight: .bold)).tracking(1.2).foregroundStyle(Theme.muted)
                    TextField("Имя аккаунта", text: $accountName).textFieldStyle(.roundedBorder)
                        .autocorrectionDisabled()
                    Text("ПАРОЛЬ").font(.system(size: 9, weight: .bold)).tracking(1.2).foregroundStyle(Theme.muted)
                    SecureField("Пароль", text: $password).textFieldStyle(.roundedBorder).onSubmit { submitPassword() }
                    Button(action: submitPassword) {
                        Text("Войти").font(.system(size: 13, weight: .semibold)).frame(maxWidth: .infinity).padding(.vertical, 12)
                    }.buttonStyle(AccentButtonStyle()).disabled(accountName.isEmpty || password.isEmpty || model.isAuthenticating)
                }.frame(width: 300, height: 235)
            }
            Text(model.authMessage).font(.system(size: 12)).foregroundStyle(Theme.muted).multilineTextAlignment(.center).frame(height: 35)
            if let error = model.errorMessage {
                Text(error).font(.system(size: 11)).foregroundStyle(.orange).multilineTextAlignment(.center)
            }
            if mode == 0 {
                Button("Обновить QR-код") { model.startQRLogin() }.buttonStyle(.plain).font(.system(size: 11)).foregroundStyle(Theme.accent)
                    .disabled(!model.coreReady)
            }
        }.padding(32).frame(width: 460).background(Theme.background)
            .onAppear { if mode == 0 && model.qrURL == nil && !model.isAuthenticating { model.startQRLogin() } }
            .onDisappear { password = ""; guardCode = "" }
    }

    private func submitPassword() {
        guard !accountName.isEmpty, !password.isEmpty else { return }
        model.passwordLogin(accountName: accountName, password: password)
        password = ""
    }
    private func submitGuard() {
        guard !guardCode.isEmpty else { return }
        model.submitGuard(guardCode)
        guardCode = ""
    }
    private func qrImage(_ text: String) -> NSImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 8, y: 8)),
              let cgImage = CIContext().createCGImage(output, from: output.extent) else { return nil }
        return NSImage(cgImage: cgImage, size: NSSize(width: 192, height: 192))
    }
}

struct GameArtwork: View {
    let url: URL?
    var body: some View {
        AsyncImage(url: url) { image in image.resizable().aspectRatio(contentMode: .fill) } placeholder: {
            ZStack {
                LinearGradient(colors: [Theme.elevated, Theme.surface], startPoint: .topLeading, endPoint: .bottomTrailing)
                Image(systemName: "gamecontroller").font(.system(size: 27)).foregroundStyle(Theme.muted.opacity(0.3))
            }
        }.clipped()
    }
}

struct AvatarView: View {
    let account: SteamAccount?
    let size: CGFloat
    var body: some View {
        AsyncImage(url: account?.avatarURL) { image in image.resizable().aspectRatio(contentMode: .fill) } placeholder: {
            ZStack {
                Theme.elevated
                Text(account?.displayName.prefix(1).uppercased() ?? "S")
                    .font(.system(size: size * 0.38, weight: .semibold, design: .rounded)).foregroundStyle(Theme.accent)
            }
        }.frame(width: size, height: size).clipShape(RoundedRectangle(cornerRadius: size * 0.3))
    }
}

struct DetailValue: View {
    let title: String
    let value: String
    let symbol: String
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.system(size: 8, weight: .semibold)).tracking(1.3).foregroundStyle(Theme.muted)
            Label(value, systemImage: symbol).font(.system(size: 11, weight: .medium))
        }
    }
}

struct EmptyState: View {
    let symbol: String
    let title: String
    let subtitle: String
    var body: some View {
        VStack(spacing: 15) {
            Image(systemName: symbol).font(.system(size: 35, weight: .light)).foregroundStyle(Theme.muted)
            Text(title).font(.system(size: 21, weight: .semibold, design: .rounded))
            Text(subtitle).font(.system(size: 12)).foregroundStyle(Theme.muted).multilineTextAlignment(.center)
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}


struct AccentButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) var enabled
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(enabled ? Theme.sidebar : Theme.muted)
            .background(enabled ? Theme.accent.opacity(configuration.isPressed ? 0.8 : 1) : Theme.elevated, in: RoundedRectangle(cornerRadius: 9))
            .scaleEffect(configuration.isPressed ? 0.98 : 1)
    }
}
