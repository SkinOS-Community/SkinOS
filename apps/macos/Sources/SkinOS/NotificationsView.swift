import AppKit
import SwiftUI

struct NotificationSettingsView: View {
    @ObservedObject var service: ClientNotifications
    let signedIn: Bool
    var body: some View {
        SettingsCard(title: "Уведомления macOS", symbol: "bell.badge") {
            VStack(alignment: .leading, spacing: 18) {
                HStack(spacing: 12) {
                    Label(service.permission.label, systemImage: service.permission == .allowed ? "checkmark.circle.fill" : "bell.slash")
                        .font(.system(size: 12)).foregroundStyle(service.permission == .allowed ? Theme.accent : Theme.muted)
                    Spacer()
                    if service.permission == .unknown {
                        Button("Разрешить уведомления") { Task { await service.requestPermissionIfNeeded() } }
                            .disabled(service.requestingPermission)
                    } else {
                        Button("Настройки macOS") { NSWorkspace.shared.open(URL(fileURLWithPath: "/System/Applications/System Settings.app")) }
                    }
                    Button { Task { await service.refreshPermission() } } label: { Image(systemName: "arrow.clockwise") }.help("Обновить состояние разрешения")
                }
                if service.permission == .allowed && (!service.systemAlertsEnabled || (service.preferences.sound && !service.systemSoundEnabled)) {
                    Text(!service.systemAlertsEnabled ? "Баннеры отключены в системных настройках SkinOS." : "Звук отключён в системных настройках SkinOS.")
                        .font(.system(size: 11)).foregroundStyle(Theme.muted)
                }
                if service.permission == .denied {
                    Text("Откройте «Уведомления → SkinOS» в настройках macOS и разрешите баннеры. Режим фокусирования также может скрывать уведомления.")
                        .font(.system(size: 11)).foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
                }
                preference("Сообщения друзей", keyPath: \.messages)
                preference("Другие уведомления Steam", keyPath: \.steam)
                preference("Загрузка завершена или прервана", keyPath: \.downloads)
                preference("Ошибки запуска игр", keyPath: \.gameErrors)
                Divider().overlay(Theme.divider)
                preference("Звук уведомлений", keyPath: \.sound)
                preference("Показывать текст уведомлений", keyPath: \.previews)
                Text("Сообщения открытого чата не показываются поверх активного окна. Настройки сохраняются сразу. Уведомления приходят, пока SkinOS работает и подключён к Steam.")
                    .font(.system(size: 11)).foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
                HStack {
                    Button("Проверить уведомление") { service.sendTest() }
                        .disabled(!signedIn || service.requestingPermission || service.permission == .denied || service.permission == .unavailable)
                    Spacer()
                    if service.requestingPermission { ProgressView().controlSize(.small) }
                }
                if let message = service.statusMessage {
                    Text(message).font(.system(size: 11)).foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
                }
            }
        }.task { await service.refreshPermission() }
    }
    private func preference(_ title: String, keyPath: WritableKeyPath<NotificationPreferences, Bool>) -> some View {
        Toggle(title, isOn: Binding(get: { service.preferences[keyPath: keyPath] }, set: { service.setPreference(keyPath, $0) }))
            .font(.system(size: 12)).toggleStyle(.switch).controlSize(.small)
    }
}

struct NotificationsView: View {
    @ObservedObject var model: AppModel
    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            HStack {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Уведомления").font(.system(size: 27, weight: .bold, design: .rounded)).tracking(-0.8)
                    Text("Сообщения Steam и события ваших игр.").font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
                Spacer()
                Button { model.section = .settings } label: { Label("Настроить", systemImage: "slider.horizontal.3") }
                Button { model.refreshNotices() } label: { Image(systemName: "arrow.clockwise") }
                    .disabled(model.account == nil || model.noticesLoading || !model.coreReady).help("Обновить уведомления Steam")
            }.buttonStyle(.bordered)
            if let error = model.noticesError {
                VStack(alignment: .leading, spacing: 9) {
                    Label("Не удалось обновить уведомления Steam", systemImage: "exclamationmark.triangle")
                        .font(.system(size: 12, weight: .medium)).foregroundStyle(.orange)
                    if !model.noticeItems.isEmpty {
                        Text("Показаны сохранённые уведомления и локальные события. Новые уведомления Steam могут отсутствовать.")
                            .font(.system(size: 11)).foregroundStyle(Theme.muted)
                    }
                    DisclosureGroup("Технические подробности") {
                        Text(error).font(.system(size: 11)).foregroundStyle(Theme.muted)
                            .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 5)
                    }.font(.system(size: 11))
                    Button("Повторить") { model.refreshNotices() }
                        .buttonStyle(.bordered).disabled(model.account == nil || model.noticesLoading || !model.coreReady)
                }.padding(13).background(Color.orange.opacity(0.07), in: RoundedRectangle(cornerRadius: 9))
            }
            if model.account == nil { WelcomeView(model: model) }
            else if model.noticesFeedState == .loading {
                ProgressView("Загружаем уведомления…").frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if model.noticesFeedState == .failed {
                EmptyState(symbol: "bell.slash", title: "Уведомления недоступны",
                           subtitle: "Не удалось проверить новые события Steam. Повторите запрос.")
            } else if model.noticesFeedState == .initial {
                EmptyState(symbol: "bell", title: "Уведомления ещё не загружены",
                           subtitle: "Нажмите кнопку обновления, чтобы получить события Steam.")
            } else if model.noticesFeedState == .empty {
                EmptyState(symbol: "bell", title: "Пока ничего нового", subtitle: "Здесь появятся уведомления Steam и сообщения о завершении загрузок.")
            } else {
                ScrollView {
                    LazyVStack(spacing: 10) {
                        if model.noticesFeedState == .refreshing {
                            ProgressView("Обновляем уведомления. Показаны последние полученные данные.")
                                .controlSize(.small).font(.system(size: 11))
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        ForEach(model.noticeItems) { notice in
                            Button { model.openNotice(notice) } label: {
                                HStack(alignment: .top, spacing: 15) {
                                    Image(systemName: symbol(notice.kind)).font(.system(size: 18)).foregroundStyle(Theme.accent)
                                        .frame(width: 34, height: 34).background(Theme.accent.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
                                    VStack(alignment: .leading, spacing: 7) {
                                        Text(notice.title).font(.system(size: 13, weight: .semibold)).foregroundStyle(.white)
                                        Text(notice.body).font(.system(size: 12)).foregroundStyle(Theme.muted).multilineTextAlignment(.leading).lineLimit(4)
                                        Text(notice.timestamp, format: .dateTime.day().month().hour().minute()).font(.system(size: 9)).foregroundStyle(Theme.muted)
                                    }
                                    Spacer(minLength: 8)
                                    Image(systemName: "chevron.right").font(.system(size: 10)).foregroundStyle(Theme.muted).padding(.top, 10)
                                }.padding(17).frame(maxWidth: .infinity, alignment: .leading).background(Theme.surface, in: RoundedRectangle(cornerRadius: 12)).contentShape(Rectangle())
                            }.buttonStyle(.plain)
                        }
                    }.padding(.bottom, 10)
                }.scrollIndicators(.hidden)
            }
        }.padding(30).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .onAppear { model.markVisibleNoticesRead(); if model.account != nil { model.refreshNotices() } }
    }
    private func symbol(_ kind: NotificationKind) -> String {
        switch kind { case .message: return "bubble.left.and.bubble.right"; case .download: return "arrow.down.circle"; case .game: return "gamecontroller"; case .steam, .test: return "bell" }
    }
}
