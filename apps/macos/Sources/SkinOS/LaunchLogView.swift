import AppKit
import Foundation
import SwiftUI

struct GameLaunchLog: Equatable {
    let appID: Int
    let logID: String?
    let text: String
    let updatedAt: Date?
    let truncated: Bool

    init?(json: [String: Any]) {
        guard let appID = json["appid"] as? Int, let text = json["text"] as? String else { return nil }
        self.appID = appID
        self.text = text
        logID = json["log_id"] as? String
        truncated = json["truncated"] as? Bool ?? false
        if let timestamp = (json["updated_at"] as? NSNumber)?.doubleValue {
            updatedAt = Date(timeIntervalSince1970: timestamp)
        } else if let timestamp = json["updated_at"] as? String {
            updatedAt = ISO8601DateFormatter().date(from: timestamp)
        } else { updatedAt = nil }
    }
}

@MainActor
extension AppModel {
    func showLaunchLog(for game: SteamGame) {
        gameForLog = game
        loadLaunchLog(for: game)
    }

    func loadLaunchLog(for game: SteamGame) {
        guard !loadingGameLogs.contains(game.id) else { return }
        gameLogErrors[game.id] = nil
        loadingGameLogs.insert(game.id)
        if let id = send("get_game_log", payload: ["appid": game.id]) {
            gameLogRequests[id] = game.id
            latestGameLogRequests[game.id] = id
        } else {
            loadingGameLogs.remove(game.id)
            gameLogErrors[game.id] = "Нет соединения с SkinOS Core. Повторите после подключения."
        }
    }

    func receiveLaunchLog(_ event: String, payload: [String: Any], requestID: String?) -> Bool {
        guard event == "game_log" || event == "error", let requestID,
              let appID = gameLogRequests.removeValue(forKey: requestID) else { return event == "game_log" }
        guard latestGameLogRequests[appID] == requestID else { return true }
        latestGameLogRequests.removeValue(forKey: appID)
        loadingGameLogs.remove(appID)
        if event == "error" {
            gameLogErrors[appID] = payload["message"] as? String ?? "Не удалось прочитать журнал запуска."
        } else if let log = GameLaunchLog(json: payload), log.appID == appID {
            gameLogs[appID] = log
            gameLogErrors[appID] = nil
        } else {
            gameLogErrors[appID] = "Получен некорректный журнал. Обновите его ещё раз."
        }
        return true
    }
}

struct LaunchLogView: View {
    @ObservedObject var model: AppModel
    let game: SteamGame
    @Environment(\.dismiss) private var dismiss
    @State private var copied = false
    private var log: GameLaunchLog? { model.gameLogs[game.id] }
    private var loading: Bool { model.loadingGameLogs.contains(game.id) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 18) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("ЖУРНАЛ ЗАПУСКА").font(.system(size: 9, weight: .semibold)).tracking(1.4).foregroundStyle(Theme.muted)
                    Text(game.name).font(.system(size: 24, weight: .bold, design: .rounded)).lineLimit(2)
                    if let updated = log?.updatedAt {
                        Text("Обновлён \(updated.formatted(date: .abbreviated, time: .standard))")
                            .font(.system(size: 10)).foregroundStyle(Theme.muted)
                    } else {
                        Text("Этапы запуска и сообщения игры").font(.system(size: 10)).foregroundStyle(Theme.muted)
                    }
                }
                Spacer()
                Button { dismiss() } label: { Image(systemName: "xmark").font(.system(size: 12)) }
                    .buttonStyle(.plain).help("Закрыть журнал").keyboardShortcut(.cancelAction)
            }.padding(24)
            Rectangle().fill(Theme.divider).frame(height: 1)
            if let error = model.gameLogErrors[game.id] {
                Text(error).font(.system(size: 12)).foregroundStyle(.orange).padding(15)
                    .frame(maxWidth: .infinity, alignment: .leading).background(Color.orange.opacity(0.07))
            }
            if loading && log == nil {
                VStack(spacing: 13) {
                    ProgressView()
                    Text("Читаем журнал…").font(.system(size: 12)).foregroundStyle(Theme.muted)
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let log, !log.text.isEmpty {
                ScrollView([.vertical, .horizontal]) {
                    Text(log.text).font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Color.white.opacity(0.86)).lineSpacing(4).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .topLeading).padding(20)
                }.background(Theme.sidebar)
            } else {
                EmptyState(symbol: "doc.text.magnifyingglass", title: "Журнал пока пуст",
                           subtitle: "Запустите игру — здесь появятся этапы запуска и подробности ошибок.")
            }
            Rectangle().fill(Theme.divider).frame(height: 1)
            HStack(spacing: 13) {
                Button { copied = false; model.loadLaunchLog(for: game) } label: {
                    HStack(spacing: 7) {
                        if loading { ProgressView().controlSize(.mini) }
                        Label("Обновить", systemImage: "arrow.clockwise")
                    }
                }.disabled(loading || !model.coreReady)
                Button(action: copyReport) { Label(copied ? "Скопировано" : "Копировать отчёт", systemImage: copied ? "checkmark" : "doc.on.doc") }
                    .disabled(log?.text.isEmpty != false)
                Spacer()
                if log?.truncated == true {
                    Text("Показана последняя часть журнала").font(.system(size: 10)).foregroundStyle(Theme.muted)
                }
            }.font(.system(size: 11)).padding(20)
        }.frame(width: 820, height: 610).background(Theme.background)
    }

    private func copyReport() {
        guard let log, !log.text.isEmpty else { return }
        // The backend returns only its sanitized report; raw helper output never enters the UI.
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(log.text, forType: .string)
        copied = true
    }
}
