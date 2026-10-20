import SwiftUI

struct DownloadsView: View {
    @ObservedObject var model: AppModel
    var body: some View {
        VStack(alignment: .leading, spacing: 23) {
            HStack(alignment: .top, spacing: 25) {
                VStack(alignment: .leading, spacing: 9) {
                    Text("Загрузки").font(.system(size: 27, weight: .bold, design: .rounded)).tracking(-0.8)
                    Text(model.activeDownloads == 0 ? "Устанавливайте и обновляйте игры из вашей библиотеки." : "\(model.runningDownloadCount) в работе · \(model.queuedDownloads.count) в очереди · \(model.downloadSpeed)")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted).monospacedDigit()
                }
                Spacer(minLength: 0)
                VStack(alignment: .trailing, spacing: 7) {
                    Text("ОДНОВРЕМЕННО").font(.system(size: 8, weight: .semibold)).tracking(1.2).foregroundStyle(Theme.muted)
                    Picker("Одновременных загрузок", selection: Binding(get: { model.maxConcurrentDownloads }, set: model.setDownloadLimit)) {
                        ForEach(1...4, id: \.self) { Text("\($0)").tag($0) }
                    }.labelsHidden().frame(width: 100).disabled(model.account == nil || !model.coreReady || model.downloadLimitPending)
                }
            }
            HStack(spacing: 14) {
                Button { model.checkUpdates() } label: {
                    HStack(spacing: 8) {
                        if model.isCheckingUpdates { ProgressView().controlSize(.small) }
                        Label("Проверить обновления", systemImage: "arrow.clockwise")
                    }.font(.system(size: 11))
                }.buttonStyle(.bordered).disabled(model.account == nil || model.isCheckingUpdates || !model.coreReady)
                Button { model.refreshDownloadQueue() } label: { Label("Обновить очередь", systemImage: "arrow.triangle.2.circlepath").font(.system(size: 11)) }
                    .buttonStyle(.plain).foregroundStyle(Theme.muted).disabled(model.account == nil || model.downloadQueueLoading || !model.coreReady)
                if let notice = model.updatesNotice { Text(notice).font(.system(size: 11)).foregroundStyle(Theme.muted).lineLimit(2) }
            }
            if let error = model.downloadQueueError {
                VStack(alignment: .leading, spacing: 10) {
                    Label("Не удалось получить или изменить очередь", systemImage: "exclamationmark.triangle")
                        .font(.system(size: 12, weight: .medium)).foregroundStyle(.orange)
                    if !model.downloads.isEmpty {
                        Text("Показана последняя полученная очередь. Она может быть устаревшей.")
                            .font(.system(size: 11)).foregroundStyle(Theme.muted)
                    }
                    DownloadFailureDetails(failure: DownloadFailurePresentation(error))
                    Button("Повторить запрос очереди") { model.refreshDownloadQueue() }
                        .buttonStyle(.bordered).disabled(model.account == nil || model.downloadQueueLoading || !model.coreReady)
                }.padding(13).background(Color.orange.opacity(0.07), in: RoundedRectangle(cornerRadius: 9))
            }
            if model.account == nil { WelcomeView(model: model) }
            else if model.downloadFeedState == .loading {
                ProgressView("Загружаем очередь…").frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 13) {
                        if model.downloadFeedState == .refreshing {
                            ProgressView("Обновляем очередь. Показаны последние полученные данные.")
                                .controlSize(.small).font(.system(size: 11))
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        ForEach(model.games.filter { $0.updateAvailable && model.downloads[$0.id]?.isActive != true }) { game in
                            HStack {
                                Label(game.name, systemImage: "arrow.down.circle").font(.system(size: 12, weight: .medium))
                                Spacer()
                                Button("Обновить") { model.install(game, update: true) }.disabled(model.launchIsActive(game) || model.pendingEnqueueGames.contains(game.id) || !model.coreReady)
                            }.padding(14).background(Theme.surface, in: RoundedRectangle(cornerRadius: 9))
                        }
                        ForEach(model.orderedDownloads) { download in
                            DownloadRow(model: model, download: download)
                        }
                        if model.downloadFeedState == .empty {
                            EmptyState(symbol: "arrow.down.to.line", title: "Очередь пуста", subtitle: "Выберите игру в библиотеке и нажмите «Установить».").frame(height: 300)
                        } else if model.downloadFeedState == .failed {
                            EmptyState(symbol: "exclamationmark.circle",
                                       title: "Очередь недоступна", subtitle: "Не удалось проверить текущие загрузки. Повторите запрос очереди.").frame(height: 220)
                        } else if model.downloadFeedState == .initial {
                            VStack(spacing: 12) {
                                EmptyState(symbol: "arrow.down.circle", title: "Очередь ещё не загружена",
                                           subtitle: "Обновите очередь, чтобы увидеть текущие загрузки.").frame(height: 200)
                                Button("Загрузить очередь") { model.refreshDownloadQueue() }
                                    .buttonStyle(.bordered).disabled(!model.coreReady)
                            }
                        }
                    }.padding(.bottom, 10)
                }.scrollIndicators(.hidden)
            }
        }.padding(30).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .onAppear { if model.account != nil { model.refreshDownloadQueue() } }
    }
}

struct DownloadRow: View {
    @ObservedObject var model: AppModel
    let download: GameDownload
    @State private var targeted = false
    private var game: SteamGame? { model.games.first { $0.id == download.id } }
    private var queuedIndex: Int? { model.queuedDownloads.firstIndex { $0.id == download.id } }
    var body: some View {
        HStack(alignment: .top, spacing: 17) {
            VStack(spacing: 8) {
                if download.phase == "queued" {
                    if model.canReorderDownload(download.id) {
                        Image(systemName: "line.3.horizontal").foregroundStyle(Theme.muted).frame(width: 24, height: 22)
                            .draggable("skinos-download:\(download.id)").help("Перетащить в очереди").accessibilityLabel("Перетащить загрузку")
                    } else {
                        Image(systemName: "line.3.horizontal").foregroundStyle(Theme.muted.opacity(0.4)).frame(width: 24, height: 22)
                    }
                    Button { model.moveDownload(download.id, by: -1) } label: { Image(systemName: "chevron.up") }
                        .disabled(queuedIndex == 0 || queuedIndex == nil || !model.canReorderDownload(download.id)).help("Выше в очереди")
                    Button { model.moveDownload(download.id, by: 1) } label: { Image(systemName: "chevron.down") }
                        .disabled(queuedIndex == model.queuedDownloads.count - 1 || queuedIndex == nil || !model.canReorderDownload(download.id)).help("Ниже в очереди")
                } else {
                    Image(systemName: download.complete ? "checkmark.circle.fill" : download.phase == "paused" ? "pause.circle" : download.failed ? "exclamationmark.circle" : download.cancelled ? "xmark.circle" : "arrow.down.circle")
                        .foregroundStyle(download.complete ? Theme.accent : download.failed ? .orange : Theme.muted).font(.system(size: 19)).frame(width: 24)
                }
            }.font(.system(size: 10, weight: .semibold)).buttonStyle(.plain).padding(.top, 5)
            GameArtwork(url: game?.headerURL).frame(width: 130, height: 61).clipShape(RoundedRectangle(cornerRadius: 8))
            VStack(alignment: .leading, spacing: 11) {
                HStack(alignment: .firstTextBaseline) {
                    Text(game?.name ?? "Игра \(download.id)").font(.system(size: 14, weight: .semibold))
                    if download.isUpdate { Text("ОБНОВЛЕНИЕ").font(.system(size: 8, weight: .semibold)).tracking(0.7).foregroundStyle(Theme.muted) }
                    Spacer()
                    DownloadControls(model: model, download: download)
                }
                DownloadMeter(download: download)
            }
        }.padding(18).background(targeted ? Theme.elevated : Theme.surface, in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12).stroke(targeted ? Theme.accent : .clear, lineWidth: 1))
            .contextMenu {
                Button("Убрать из очереди") { model.controlDownload("remove_download", appID: download.id) }
                    .disabled(model.account == nil || !model.coreReady || model.pendingDownloadControls.contains(download.id))
            }
            .dropDestination(for: String.self) { items, _ in
                guard download.phase == "queued", let item = items.first, item.hasPrefix("skinos-download:"),
                      let appID = Int(item.dropFirst("skinos-download:".count)), appID != download.id,
                      model.canReorderDownload(appID) else { return false }
                model.moveDownload(appID, to: download.queuePosition)
                return true
            } isTargeted: { targeted = $0 && download.phase == "queued" }
    }
}

struct DownloadControls: View {
    @ObservedObject var model: AppModel
    let download: GameDownload
    private var pending: Bool { model.pendingDownloadControls.contains(download.id) || model.pendingEnqueueGames.contains(download.id) }
    var body: some View {
        HStack(spacing: 10) {
            if pending { ProgressView().controlSize(.mini) }
            if download.canPause {
                Button { model.controlDownload("pause_download", appID: download.id) } label: { Label("Пауза", systemImage: "pause.fill") }
            }
            if download.canResume {
                Button { model.controlDownload("resume_download", appID: download.id) } label: { Label("Продолжить", systemImage: "play.fill") }
            }
            if download.canCancel {
                Button { model.controlDownload("cancel_download", appID: download.id) } label: { Image(systemName: "xmark.circle") }
                    .help("Отменить загрузку").accessibilityLabel("Отменить загрузку")
            }
            if download.canRetry {
                if download.failed, DownloadFailurePresentation(download.message).action == .inspectAccount {
                    Button("Проверить аккаунт") { model.section = .account }
                }
                Button { model.controlDownload("resume_download", appID: download.id) } label: { Label("Повторить", systemImage: "arrow.clockwise") }
            }
            Menu {
                Button { model.controlDownload("remove_download", appID: download.id) } label: { Label("Убрать из очереди", systemImage: "minus.circle") }
            } label: {
                Image(systemName: "ellipsis").frame(width: 12)
            }.menuStyle(.borderlessButton).fixedSize().help("Действия с загрузкой")
        }.font(.system(size: 11)).buttonStyle(.bordered)
            .disabled(pending || !model.coreReady || model.account == nil)
    }
}

struct DownloadMeter: View {
    let download: GameDownload
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let platform = download.platform {
                Label(platform.downloadLabel, systemImage: platform.symbol)
                    .font(.system(size: 10)).foregroundStyle(Theme.muted)
            }
            HStack {
                Text(download.failed ? "Ошибка загрузки" : download.complete ? "Готово" : download.phaseDescription).lineLimit(2)
                Spacer()
                if let progress = download.displayProgress { Text("\(Int(progress * 100))%").monospacedDigit() }
            }.font(.system(size: 11, weight: .medium)).foregroundStyle(download.failed ? .orange : download.complete ? Theme.accent : .white)
            if download.isActive {
                if download.isRunning || download.displayProgress != nil {
                    ProgressView(value: download.displayProgress).controlSize(.small).tint(download.phase == "paused" ? Theme.muted : Theme.accent)
                }
                Text(download.byteDescription).font(.system(size: 10)).foregroundStyle(Theme.muted).monospacedDigit()
                if download.isNetworkPhase {
                    ViewThatFits(in: .horizontal) {
                        HStack {
                            Text(download.speedDescription).monospacedDigit()
                            Spacer(minLength: 15)
                            Text(download.etaDescription)
                        }
                        VStack(alignment: .leading, spacing: 5) {
                            Text(download.speedDescription).monospacedDigit()
                            Text(download.etaDescription)
                        }
                    }.font(.system(size: 10)).foregroundStyle(Theme.muted)
                }
                if download.isDiskPhase, download.totalFiles > 0 {
                    Text("Файлы: \(download.filesCompleted) из \(download.totalFiles)")
                        .font(.system(size: 9)).foregroundStyle(Theme.muted).monospacedDigit()
                }
                if download.totalChunks > 0, ["verifying", "downloading"].contains(download.phase) {
                    Text("Блоки: \(download.chunksCompleted) из \(download.totalChunks)")
                        .font(.system(size: 9)).foregroundStyle(Theme.muted).monospacedDigit()
                }
            } else if download.totalBytes > 0 || download.installTotalBytes > 0 {
                Text(download.byteDescription).font(.system(size: 10)).foregroundStyle(Theme.muted)
            }
            if download.failed {
                DownloadFailureDetails(failure: DownloadFailurePresentation(download.message))
            } else if let message = download.message, !message.isEmpty {
                Text(message).font(.system(size: 10)).foregroundStyle(Theme.muted)
                    .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            }
            if download.reusedBytes > 0, !download.failed {
                Text("Использовано с диска: \(GameDownload.bytes(download.reusedBytes))")
                    .font(.system(size: 9)).foregroundStyle(Theme.muted)
            }
        }
    }
}

private struct DownloadFailureDetails: View {
    let failure: DownloadFailurePresentation
    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(failure.title).font(.system(size: 11, weight: .medium)).foregroundStyle(.orange)
            Text(failure.guidance).font(.system(size: 11)).foregroundStyle(Theme.muted)
                .fixedSize(horizontal: false, vertical: true)
            if let diagnostic = failure.diagnostic {
                DisclosureGroup("Технические подробности") {
                    Text(diagnostic).font(.system(size: 10, design: .monospaced)).foregroundStyle(Theme.muted)
                        .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 5)
                }.font(.system(size: 11))
            }
        }
    }
}
