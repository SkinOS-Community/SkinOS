import AppKit
import Combine
import Foundation
import Sparkle
import SwiftUI

/// Release configuration is embedded before code signing. No remote response or
/// user-defaults value may supply a replacement public key.
struct AppUpdateConfiguration: Equatable {
    let feedURL: URL

    init?(info: [String: Any]) {
        guard let feed = info["SUFeedURL"] as? String,
              let url = URL(string: feed), url.scheme == "https", url.host != nil,
              url.user == nil, url.password == nil, url.fragment == nil,
              let key = info["SUPublicEDKey"] as? String,
              Data(base64Encoded: key)?.count == 32,
              info["SUVerifyUpdateBeforeExtraction"] as? Bool == true,
              info["SURequireSignedFeed"] as? Bool == true,
              (info["SUSignedFeedFailureExpirationInterval"] as? NSNumber)?.doubleValue == 0,
              info["SUAllowsAutomaticUpdates"] as? Bool == false else { return nil }
        feedURL = url
    }
}

@MainActor
extension AppModel {
    var appUpdateBlockReason: String? {
        if anyGameActive { return "Закройте запущенную игру перед установкой обновления SkinOS." }
        if downloads.values.contains(where: { $0.isRunning || ($0.phase == "queued" && $0.isActive) })
            || !pendingEnqueueGames.isEmpty || !pendingDownloadControls.isEmpty {
            return "Приостановите загрузки игр перед установкой обновления SkinOS."
        }
        if !uninstallingGames.isEmpty || steamInputBusy {
            return "Дождитесь завершения текущей операции с игрой."
        }
        if !coreReady || (account != nil && !downloadQueueReady) {
            return "Дождитесь подключения клиента и загрузки очереди."
        }
        return nil
    }
}

/// Sparkle owns network, signature verification, update UI and its preferences.
/// This adapter only supplies product policy and presents the existing controls.
@MainActor
final class AppUpdates: NSObject, ObservableObject, SPUUpdaterDelegate {
    @Published private(set) var canCheckForUpdates = false
    @Published private(set) var automaticallyChecksForUpdates = false
    @Published private(set) var lastUpdateCheckDate: Date?
    @Published private(set) var notice: String?
    @Published private(set) var pendingInstallation = false
    @Published private(set) var isConfigured = false
    private(set) var installationRequested = false
    private weak var model: AppModel?
    private var configuration: AppUpdateConfiguration?
    private var controller: SPUStandardUpdaterController?
    private var installHandler: (() -> Void)?
    private var started = false

    init(model: AppModel? = nil) {
        self.model = model
        super.init()
    }

    func start(model: AppModel, bundle: Bundle = .main) {
        guard !started else { return }
        started = true
        self.model = model
        guard let configuration = AppUpdateConfiguration(info: bundle.infoDictionary ?? [:]) else {
            notice = "Автообновления не настроены для этой сборки."
            return
        }
        self.configuration = configuration
        let controller = SPUStandardUpdaterController(startingUpdater: false,
            updaterDelegate: self, userDriverDelegate: nil)
        self.controller = controller
        controller.updater.publisher(for: \.canCheckForUpdates).assign(to: &$canCheckForUpdates)
        controller.updater.publisher(for: \.automaticallyChecksForUpdates).assign(to: &$automaticallyChecksForUpdates)
        controller.updater.publisher(for: \.lastUpdateCheckDate).assign(to: &$lastUpdateCheckDate)
        do {
            try controller.updater.start()
            isConfigured = true
        } catch {
            notice = "Не удалось запустить проверку обновлений: \(error.localizedDescription)"
        }
    }

    func checkForUpdates() {
        guard canCheckForUpdates else { return }
        notice = nil
        controller?.checkForUpdates(nil)
    }

    func setAutomaticChecks(_ enabled: Bool) {
        guard isConfigured else { return }
        controller?.updater.automaticallyChecksForUpdates = enabled
    }

    // Keep the signed-in-bundle feed authoritative instead of allowing a stale
    // Sparkle user-defaults override from a developer build to redirect it.
    func feedURLString(for updater: SPUUpdater) -> String? { configuration?.feedURL.absoluteString }

    func updater(_ updater: SPUUpdater, shouldPostponeRelaunchForUpdate item: SUAppcastItem,
                 untilInvokingBlock installHandler: @escaping () -> Void) -> Bool {
        postponeInstallationIfBusy(installHandler)
    }

    func postponeInstallationIfBusy(_ installHandler: @escaping () -> Void) -> Bool {
        installationRequested = true
        guard let reason = model?.appUpdateBlockReason else { return false }
        self.installHandler = installHandler
        pendingInstallation = true
        notice = reason
        return true
    }

    func installPendingUpdate() {
        guard let installHandler else { return }
        if let reason = model?.appUpdateBlockReason { notice = reason; return }
        self.installHandler = nil
        pendingInstallation = false
        notice = nil
        installHandler()
    }

    /// Last check at AppKit's termination boundary also covers a game/download
    /// starting after Sparkle's relaunch delegate returned.
    func shouldAllowApplicationTermination() -> Bool {
        guard installationRequested, let reason = model?.appUpdateBlockReason else { return true }
        notice = reason
        model?.section = .settings
        model?.activateApplication()
        return false
    }

    func updater(_ updater: SPUUpdater, didAbortWithError error: Error) {
        installationRequested = false
        pendingInstallation = false
        installHandler = nil
        notice = error.localizedDescription
    }

    func updaterDidNotFindUpdate(_ updater: SPUUpdater) {
        notice = "Установлена последняя версия SkinOS."
    }
}

struct AppUpdateSettingsView: View {
    @ObservedObject var updates: AppUpdates
    @ObservedObject var model: AppModel

    var body: some View {
        SettingsCard(title: "Обновления SkinOS", symbol: "arrow.triangle.2.circlepath") {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("Версия \(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "—")")
                        .font(.system(size: 12, weight: .medium))
                    Spacer()
                    Button("Проверить обновления…") { updates.checkForUpdates() }
                        .disabled(!updates.canCheckForUpdates)
                }
                Toggle("Проверять обновления автоматически", isOn: Binding(
                    get: { updates.automaticallyChecksForUpdates }, set: updates.setAutomaticChecks))
                    .toggleStyle(.switch).controlSize(.small).font(.system(size: 12))
                    .disabled(!updates.isConfigured)
                Text("Обновления загружаются с GitHub и проверяются по цифровой подписи. Установка — с вашим подтверждением, когда игры и загрузки не выполняются.")
                    .font(.system(size: 11)).foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
                if let date = updates.lastUpdateCheckDate {
                    Text("Последняя проверка: \(date.formatted(date: .abbreviated, time: .shortened))")
                        .font(.system(size: 11)).foregroundStyle(Theme.muted)
                }
                if let notice = updates.notice {
                    Text(notice).font(.system(size: 11)).foregroundStyle(Theme.muted)
                        .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
                }
                if updates.pendingInstallation {
                    Button("Установить и перезапустить SkinOS") { updates.installPendingUpdate() }
                        .disabled(model.appUpdateBlockReason != nil)
                    if let reason = model.appUpdateBlockReason {
                        Text(reason).font(.system(size: 11)).foregroundStyle(.orange)
                    }
                }
            }
        }
    }
}
