import SwiftUI

struct LibraryFilters: View {
    @ObservedObject var model: AppModel
    private var active: Bool { model.filter != .all || model.installFilter != .all || model.playFilter != .all }
    var body: some View {
        VStack(alignment: .leading, spacing: 13) {
            HStack(spacing: 6) {
                ForEach(GameFilter.allCases) { filter in
                    Button { model.filter = filter } label: {
                        Text(filter.rawValue).font(.system(size: 11, weight: .medium))
                            .padding(.horizontal, 12).padding(.vertical, 8)
                            .foregroundStyle(model.filter == filter ? .white : Theme.muted)
                            .background(model.filter == filter ? Theme.elevated : .clear, in: Capsule())
                    }.buttonStyle(.plain)
                }
                Spacer(minLength: 0)
                Text("\(model.filteredGames.count)").font(.system(size: 10, design: .monospaced)).foregroundStyle(Theme.muted)
            }
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) { selectionMenus; Spacer(minLength: 0); sortMenu }
                VStack(alignment: .leading, spacing: 12) { HStack(spacing: 12) { selectionMenus }; sortMenu }
            }
        }.padding(.bottom, 23)
    }
    @ViewBuilder private var selectionMenus: some View {
        Picker("Установка", selection: $model.installFilter) { ForEach(InstallFilter.allCases) { Text($0.rawValue).tag($0) } }
            .labelsHidden().fixedSize().help("Фильтр по установке")
        Picker("Время игры", selection: $model.playFilter) { ForEach(PlayFilter.allCases) { Text($0.rawValue).tag($0) } }
            .labelsHidden().fixedSize().help("Фильтр по времени игры и избранному")
        if active {
            Button { model.filter = .all; model.installFilter = .all; model.playFilter = .all } label: { Image(systemName: "xmark.circle.fill") }
                .buttonStyle(.plain).foregroundStyle(Theme.muted).help("Сбросить фильтры")
        }
    }
    private var sortMenu: some View {
        Picker("Сортировка", selection: $model.librarySort) { ForEach(LibrarySort.allCases) { Text($0.rawValue).tag($0) } }
            .labelsHidden().fixedSize().help("Сортировка библиотеки")
    }
}

struct RequirementsView: View {
    @ObservedObject var model: AppModel
    let game: SteamGame
    var body: some View {
        VStack(alignment: .leading, spacing: 15) {
            Text("СИСТЕМНЫЕ ТРЕБОВАНИЯ").font(.system(size: 9, weight: .bold)).tracking(1.1).foregroundStyle(Theme.muted)
            if model.detailsLoading.contains(game.id) { ProgressView().controlSize(.small) }
            else if let details = model.appDetails[game.id] {
                if details.requirements.isEmpty {
                    Text("Издатель не указал поддерживаемые платформы в магазине.").font(.system(size: 11)).foregroundStyle(Theme.muted)
                }
                ForEach(details.requirements) { platform in
                    DisclosureGroup(platform.title) {
                        VStack(alignment: .leading, spacing: 15) {
                            if !platform.minimum.isEmpty {
                                requirement(title: "Минимальные", text: platform.minimum)
                            }
                            if !platform.recommended.isEmpty {
                                requirement(title: "Рекомендуемые", text: platform.recommended)
                            }
                            if platform.minimum.isEmpty && platform.recommended.isEmpty {
                                Text("Издатель не указал требования для этой платформы.").font(.system(size: 11)).foregroundStyle(Theme.muted)
                            }
                        }.padding(.top, 10).padding(.bottom, 7).frame(maxWidth: .infinity, alignment: .leading)
                    }.font(.system(size: 12, weight: .semibold)).tint(Theme.muted)
                }
            } else {
                Text(model.detailsErrors[game.id] ?? "Требования пока не загружены.").font(.system(size: 11)).foregroundStyle(Theme.muted)
                Button("Загрузить требования") { model.loadDetails(game, force: true) }.buttonStyle(.plain).font(.system(size: 11)).foregroundStyle(Theme.accent)
            }
        }.onAppear { model.loadDetails(game) }.onChange(of: game.id) { _, _ in model.loadDetails(game) }
    }
    private func requirement(title: String, text: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.system(size: 10, weight: .semibold)).foregroundStyle(.white)
            Text(text).font(.system(size: 10)).lineSpacing(4).foregroundStyle(Theme.muted).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
        }
    }
}
