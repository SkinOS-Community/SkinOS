import SwiftUI

struct DownloadRegionPicker: View {
    @Binding var selection: UInt32?
    let regions: [DownloadRegion]
    @State private var isPresented = false
    @State private var search = ""
    @FocusState private var searchFocused: Bool

    private var selectedLabel: String { DownloadRegion.selectionLabel(selection, regions: regions) }
    private var matches: [DownloadRegion] { DownloadRegion.matching(regions, query: search) }

    var body: some View {
        Button { search = ""; isPresented = true } label: {
            HStack(spacing: 10) {
                Text(selectedLabel).lineLimit(2).multilineTextAlignment(.leading)
                Spacer(minLength: 4)
                Image(systemName: "chevron.up.chevron.down").font(.system(size: 9, weight: .semibold))
            }
            .font(.system(size: 12)).foregroundStyle(Theme.accent)
            .padding(.horizontal, 12).padding(.vertical, 9)
            .background(Theme.accent.opacity(0.07), in: RoundedRectangle(cornerRadius: 7))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Регион загрузки")
        .accessibilityValue(selectedLabel)
        .help(selectedLabel)
        .popover(isPresented: $isPresented, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 8) {
                    Image(systemName: "magnifyingglass").foregroundStyle(Theme.muted)
                    TextField("Найти страну или город", text: $search)
                        .textFieldStyle(.plain).focused($searchFocused)
                        .accessibilityLabel("Поиск региона загрузки")
                    if !search.isEmpty {
                        Button { search = "" } label: { Image(systemName: "xmark.circle.fill") }
                            .buttonStyle(.plain).foregroundStyle(Theme.muted)
                            .accessibilityLabel("Очистить поиск регионов")
                    }
                }
                .font(.system(size: 12)).padding(14)
                Divider().overlay(Theme.divider)
                regionRow(id: nil, name: "Автоматически", symbol: "location")
                    .padding(6)
                Divider().overlay(Theme.divider)
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        if regions.isEmpty {
                            Text("Список регионов пока недоступен. Сохранённый выбор не изменён.")
                                .font(.system(size: 11)).foregroundStyle(Theme.muted)
                                .padding(12).fixedSize(horizontal: false, vertical: true)
                        } else if matches.isEmpty {
                            Text("Регион не найден")
                                .font(.system(size: 12)).foregroundStyle(Theme.muted)
                                .padding(12)
                        } else {
                            ForEach(matches) { region in
                                regionRow(id: region.id, name: region.name, symbol: "globe")
                            }
                        }
                    }.padding(6)
                }
                .frame(height: 280)
            }
            .frame(width: 340)
            .background(Theme.surface)
            .onAppear { searchFocused = true }
            .onExitCommand { isPresented = false }
        }
    }

    private func regionRow(id: UInt32?, name: String, symbol: String) -> some View {
        Button {
            selection = id
            isPresented = false
        } label: {
            HStack(spacing: 10) {
                Image(systemName: symbol).foregroundStyle(Theme.muted).frame(width: 16)
                Text(name).multilineTextAlignment(.leading)
                Spacer(minLength: 4)
                if selection == id { Image(systemName: "checkmark").foregroundStyle(Theme.accent) }
            }
            .font(.system(size: 12)).padding(.horizontal, 10).padding(.vertical, 9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
            .background(selection == id ? Theme.accent.opacity(0.08) : .clear,
                        in: RoundedRectangle(cornerRadius: 6))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selection == id ? .isSelected : [])
    }
}
