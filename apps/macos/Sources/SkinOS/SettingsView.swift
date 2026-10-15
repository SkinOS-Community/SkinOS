import AppKit
import SwiftUI

struct SettingsView: View {
    @ObservedObject var model: AppModel
    @EnvironmentObject private var updates: AppUpdates
    @State private var draft = ClientSettings()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 25) {
                VStack(alignment: .leading, spacing: 9) {
                    Text("Настроено под вас.")
                        .font(.system(size: 27, weight: .bold, design: .rounded)).tracking(-0.8)
                    Text("Библиотека, загрузки и запуск Windows-игр на Mac.")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
                NotificationSettingsView(service: model.notifications, signedIn: model.account != nil)
                AppUpdateSettingsView(updates: updates, model: model)
                SettingsCard(title: "Библиотека", symbol: "internaldrive") {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("Каталог steamapps").font(.system(size: 12, weight: .medium))
                        HStack(spacing: 10) {
                            TextField("Путь к steamapps", text: $draft.steamappsDirectory)
                                .textFieldStyle(.roundedBorder).font(.system(size: 11, design: .monospaced))
                            Button("Выбрать…", action: chooseLibrary)
                        }
                        Text("Игры хранятся в common внутри этого каталога. Уже установленные игры появятся в библиотеке.")
                            .font(.system(size: 11)).foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
                    }
                }
                SettingsCard(title: "Загрузки", symbol: "arrow.down.circle") {
                    VStack(alignment: .leading, spacing: 13) {
                        HStack(alignment: .center, spacing: 18) {
                            VStack(alignment: .leading, spacing: 6) {
                                Text("Регион загрузки").font(.system(size: 12, weight: .medium))
                                Text("Выберите регион серверов Steam или доверьте выбор Steam.")
                                    .font(.system(size: 11)).foregroundStyle(Theme.muted)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            Spacer(minLength: 0)
                            DownloadRegionPicker(selection: $draft.downloadRegionID, regions: model.downloadRegions)
                                .frame(width: 255)
                        }
                        Text("Изменение применяется после сохранения к новым и возобновлённым загрузкам. Чтобы сменить регион текущей загрузки, приостановите её и продолжите. Если регион недоступен, используются резервные серверы Steam.")
                            .font(.system(size: 11)).foregroundStyle(Theme.muted)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                SettingsCard(title: "Windows-игры", symbol: "desktopcomputer") {
                    VStack(alignment: .leading, spacing: 18) {
                        HStack(alignment: .top) {
                            VStack(alignment: .leading, spacing: 6) {
                                Text("CrossOver по умолчанию").font(.system(size: 12, weight: .medium))
                                Text("Для каждой игры можно выбрать другую версию.")
                                    .font(.system(size: 11)).foregroundStyle(Theme.muted)
                            }
                            Spacer()
                            CrossOverPicker(selection: $draft.crossoverPath, installations: model.crossovers, inheritedLabel: "Автоматически")
                                .frame(width: 245)
                        }
                        if model.crossovers.isEmpty {
                            Label("CrossOver не найден. Установите CrossOver или CrossOver Preview для запуска Windows-игр.", systemImage: "info.circle")
                                .font(.system(size: 11)).foregroundStyle(.orange).fixedSize(horizontal: false, vertical: true)
                        } else {
                            VStack(alignment: .leading, spacing: 8) {
                                ForEach(model.crossovers) { installation in
                                    HStack(spacing: 9) {
                                        Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.accent)
                                        Text(installation.label)
                                        Spacer()
                                        Text(installation.path).lineLimit(1).truncationMode(.middle).foregroundStyle(Theme.muted)
                                    }.font(.system(size: 11))
                                }
                            }
                        }
                        Divider().overlay(Theme.divider)
                        HStack {
                            VStack(alignment: .leading, spacing: 7) {
                                Text("NotProton").font(.system(size: 13, weight: .semibold))
                                Text("Встроен в клиент · обновляется вместе со SkinOS")
                                    .font(.system(size: 11)).foregroundStyle(Theme.muted)
                            }
                            Spacer()
                            Text(model.notProton.label).font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.accent)
                        }
                    }
                }
                HStack(spacing: 15) {
                    Button { model.saveSettings(draft) } label: {
                        HStack(spacing: 8) {
                            if model.isSavingSettings { ProgressView().controlSize(.small) }
                            Text("Сохранить настройки").font(.system(size: 12, weight: .semibold))
                        }.padding(.horizontal, 18).padding(.vertical, 11)
                    }.buttonStyle(AccentButtonStyle())
                        .disabled(!model.coreReady || !model.settingsLoaded || model.isSavingSettings || draft.steamappsDirectory.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    Button("Проверить установленные версии") { model.loadSettings() }
                        .buttonStyle(.plain).font(.system(size: 11)).foregroundStyle(Theme.muted)
                    Spacer()
                    if let notice = model.settingsNotice {
                        Label(notice, systemImage: "checkmark.circle").font(.system(size: 11)).foregroundStyle(Theme.accent)
                    }
                }
            }.padding(30).frame(maxWidth: 900, alignment: .leading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear { draft = model.settings }
        .onChange(of: model.settings) { _, settings in draft = settings }
    }

    private func chooseLibrary() {
        let panel = NSOpenPanel()
        panel.title = "Выберите каталог steamapps"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.directoryURL = URL(fileURLWithPath: draft.steamappsDirectory, isDirectory: true)
        panel.begin { response in
            if response == .OK, let url = panel.url { draft.steamappsDirectory = url.path }
        }
    }
}

struct GameSettingsView: View {
    @ObservedObject var model: AppModel
    let game: SteamGame
    @State private var draft = GameSettings()

    private var windowsSelected: Bool {
        draft.compatibility == "notproton" || (draft.compatibility == "auto" && (!game.supportsMacOS || game.installedPlatform == "windows"))
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("СВОЙСТВА ИГРЫ").font(.system(size: 9, weight: .semibold)).tracking(1.4).foregroundStyle(Theme.muted)
                    Text(game.name).font(.system(size: 23, weight: .bold, design: .rounded)).lineLimit(2)
                }
                Spacer()
                Button { model.gameForSettings = nil } label: { Image(systemName: "xmark").font(.system(size: 12)) }
                    .buttonStyle(.plain).accessibilityLabel("Закрыть свойства игры")
            }.padding(25)
            Divider().overlay(Theme.divider)
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    SettingsCard(title: "Запуск", symbol: "play.circle") {
                        VStack(alignment: .leading, spacing: 16) {
                            HStack {
                                Text("Версия игры").font(.system(size: 12))
                                Spacer()
                                Picker("Версия игры", selection: $draft.compatibility) {
                                    Text("Автоматически").tag("auto")
                                    if game.supportsMacOS { Text("Нативная macOS").tag("native") }
                                    if game.supportsWindows || game.installedPlatform == "windows" { Text("Windows · NotProton").tag("notproton") }
                                }.labelsHidden().frame(width: 225)
                            }
                            VStack(alignment: .leading, spacing: 9) {
                                Text("Параметры запуска").font(.system(size: 12))
                                TextField("Например: -windowed", text: $draft.launchArguments)
                                    .textFieldStyle(.roundedBorder).font(.system(size: 12, design: .monospaced))
                                Text("Дополнительные аргументы для этой игры. Пути с пробелами заключайте в кавычки.")
                                    .font(.system(size: 10)).foregroundStyle(Theme.muted)
                            }
                        }
                    }
                    if windowsSelected {
                        SettingsCard(title: "CrossOver", symbol: "desktopcomputer") {
                            VStack(alignment: .leading, spacing: 18) {
                                HStack {
                                    Text("Приложение").font(.system(size: 12))
                                    Spacer()
                                    CrossOverPicker(selection: $draft.crossoverPath, installations: model.crossovers, inheritedLabel: "Из настроек SkinOS")
                                        .frame(width: 225)
                                }
                                Text("У каждой версии CrossOver отдельная среда. При первом переключении настройки и сохранения копируются; затем прогресс хранится отдельно.")
                                    .font(.system(size: 10)).foregroundStyle(Theme.muted)
                                    .fixedSize(horizontal: false, vertical: true)
                                HStack {
                                    Text("Графика").font(.system(size: 12))
                                    Spacer()
                                    Picker("Графика", selection: $draft.graphics) {
                                        Text("Автоматически").tag("auto")
                                        Text("D3DMetal").tag("d3dmetal")
                                        Text("DXMT").tag("dxmt")
                                        Text("DXVK").tag("dxvk")
                                        Text("WineD3D").tag("wined3d")
                                    }.labelsHidden().frame(width: 225)
                                }
                                HStack {
                                    Text("Синхронизация").font(.system(size: 12))
                                    Spacer()
                                    Picker("Синхронизация", selection: $draft.sync) {
                                        Text("Автоматически").tag("auto")
                                        Text("MSync").tag("msync")
                                        Text("Без ускорения").tag("none")
                                    }.labelsHidden().frame(width: 225)
                                }
                                Toggle("Высокое разрешение Retina", isOn: $draft.highResolution)
                                    .font(.system(size: 12)).toggleStyle(.switch).controlSize(.small)
                                Toggle("Steam Input · экспериментально", isOn: $draft.steamInput)
                                    .font(.system(size: 12)).toggleStyle(.switch).controlSize(.small)
                                    .help("Управление геймпадом через Steam Input для этой игры.")
                                if draft.steamInput {
                                    Text("Подключите один контроллер до запуска игры. Повторное подключение контроллера той же модели восстанавливается автоматически. Поддержка вибрации зависит от игры и контроллера.")
                                        .font(.system(size: 10)).foregroundStyle(Theme.muted)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                                if model.crossovers.isEmpty {
                                    Text("Для запуска нужен установленный CrossOver или CrossOver Preview.")
                                        .font(.system(size: 11)).foregroundStyle(.orange)
                                }
                            }
                        }
                    }
                    if windowsSelected && game.isInstalled {
                        SteamInputDiagnosticCard(model: model, game: game,
                            hasUnsavedChanges: draft != (model.gameSettings[game.id] ?? GameSettings()))
                    }
                    GameUninstallButton(model: model, game: game)
                    if let notice = model.gameSettingsNotice {
                        Label(notice, systemImage: "checkmark.circle").font(.system(size: 11)).foregroundStyle(Theme.accent)
                    }
                    if let error = model.errorMessage {
                        Text(error).font(.system(size: 11)).foregroundStyle(.orange).textSelection(.enabled)
                    }
                }.padding(25)
            }
            Divider().overlay(Theme.divider)
            HStack {
                Text("Применяются при следующем запуске").font(.system(size: 10)).foregroundStyle(Theme.muted)
                Spacer()
                Button { model.saveGameSettings(draft, for: game) } label: {
                    HStack(spacing: 8) {
                        if model.isSavingGameSettings || model.loadingGameSettings.contains(game.id) { ProgressView().controlSize(.small) }
                        Text("Сохранить").font(.system(size: 12, weight: .semibold))
                    }.padding(.horizontal, 23).padding(.vertical, 11)
                }.buttonStyle(AccentButtonStyle())
                    .disabled(!model.coreReady || model.isSavingGameSettings || model.loadingGameSettings.contains(game.id))
            }.padding(25)
        }
        .frame(width: 610, height: windowsSelected ? 660 : 440)
        .background(Theme.background)
        .onAppear { draft = model.gameSettings[game.id] ?? GameSettings() }
        .onChange(of: model.gameSettings[game.id]) { _, value in if let value { draft = value } }
    }
}

struct CrossOverPicker: View {
    @Binding var selection: String
    let installations: [CrossOverInstallation]
    let inheritedLabel: String
    var body: some View {
        Picker("CrossOver", selection: $selection) {
            Text(inheritedLabel).tag("")
            ForEach(installations) { installation in Text(installation.label).tag(installation.path) }
            if !selection.isEmpty, !installations.contains(where: { $0.path == selection }) {
                Text("Недоступен: \(URL(fileURLWithPath: selection).deletingPathExtension().lastPathComponent)").tag(selection)
            }
        }.labelsHidden()
    }
}

struct SettingsCard<Content: View>: View {
    let title: String
    let symbol: String
    @ViewBuilder var content: () -> Content
    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Label(title, systemImage: symbol).font(.system(size: 14, weight: .semibold)).foregroundStyle(Theme.accent)
            content()
        }.padding(22).frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: 13))
    }
}
