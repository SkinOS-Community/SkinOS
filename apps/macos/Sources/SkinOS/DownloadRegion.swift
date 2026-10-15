import CoreFoundation
import Foundation

struct DownloadRegion: Identifiable, Equatable {
    let id: UInt32
    let name: String

    init?(json: [String: Any]) {
        guard let id = Self.parseID(json["id"]),
              let name = (json["name"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
              !name.isEmpty else { return nil }
        self.id = id
        self.name = name
    }

    // Steam cell 0 means automatic discovery, so it is never a selectable region.
    static func parseID(_ value: Any?) -> UInt32? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID(),
              let id = UInt32(number.stringValue), id > 0 else { return nil }
        return id
    }

    static func catalog(json: [[String: Any]]) -> [DownloadRegion] {
        var seen = Set<UInt32>()
        return json.compactMap(Self.init(json:)).filter { seen.insert($0.id).inserted }
            .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    static func selectionLabel(_ id: UInt32?, regions: [DownloadRegion]) -> String {
        guard let id else { return "Автоматически" }
        return regions.first(where: { $0.id == id })?.name ?? "Сохранённый регион №\(id)"
    }

    static func matching(_ regions: [DownloadRegion], query: String) -> [DownloadRegion] {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return query.isEmpty ? regions : regions.filter { $0.name.localizedStandardContains(query) }
    }
}
