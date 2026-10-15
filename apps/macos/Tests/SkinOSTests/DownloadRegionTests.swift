import Foundation
import Testing
@testable import SkinOS

@MainActor
struct DownloadRegionTests {
    @Test func automaticDefaultAndRegionRoundTripUseNullableWireField() throws {
        for payload: [String: Any] in [[:], ["download_region_id": NSNull()], ["download_region_id": 0]] {
            let settings = ClientSettings(json: payload)
            #expect(settings.downloadRegionID == nil)
            #expect(settings.payload["download_region_id"] is NSNull)
        }
        var settings = ClientSettings()
        settings.downloadRegionID = 143
        let encoded = try JSONSerialization.data(withJSONObject: settings.payload)
        let decoded = try #require(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
        #expect(ClientSettings(json: decoded).downloadRegionID == 143)
        settings.downloadRegionID = nil
        #expect(settings.payload["download_region_id"] is NSNull)
    }

    @Test func malformedCatalogCannotCreateInvalidSelections() {
        let catalog = DownloadRegion.catalog(json: [
            ["id": 143, "name": "  Sweden - Stockholm "],
            ["id": 12, "name": "Germany - Frankfurt"],
            ["id": 143, "name": "Duplicate"],
            ["id": 0, "name": "Automatic"],
            ["id": -1, "name": "Invalid"],
            ["id": UInt64(UInt32.max) + 1, "name": "Overflow"],
            ["id": true, "name": "Boolean"],
            ["id": 2.5, "name": "Fractional"],
            ["id": 23, "name": "  "],
            ["name": "Missing ID"]
        ])
        #expect(catalog.map(\.id) == [12, 143])
        #expect(catalog.last?.name == "Sweden - Stockholm")
        #expect(DownloadRegion.matching(catalog, query: " sTOckHOLM ").map(\.id) == [143])
        #expect(DownloadRegion.matching(catalog, query: "missing").isEmpty)
        #expect(DownloadRegion.matching(catalog, query: " ") == catalog)
    }

    @Test func unknownSavedRegionRemainsVisibleAndIsNotSilentlyReset() {
        let settings = ClientSettings(json: ["download_region_id": 555])
        #expect(settings.downloadRegionID == 555)
        #expect(DownloadRegion.selectionLabel(settings.downloadRegionID, regions: []).contains("555"))
        #expect(ClientSettings(json: settings.payload).downloadRegionID == 555)
        #expect(DownloadRegion.selectionLabel(nil, regions: []) == "Автоматически")
    }

    @Test func settingsEventUpdatesCatalogAndSaveDoesNotControlActiveTransfers() {
        let model = AppModel()
        model.coreReady = true
        var commands: [(String, [String: Any])] = []
        model.commandSink = { command, payload in
            commands.append((command, payload))
            return "fixture-\(command)"
        }
        model.receive(["event": "settings", "payload": [
            "settings": ["download_region_id": 143],
            "download_regions": [["id": 143, "name": "Sweden - Stockholm"]]
        ]])
        #expect(model.settings.downloadRegionID == 143)
        #expect(model.downloadRegions.map(\.name) == ["Sweden - Stockholm"])
        #expect(DownloadRegion.selectionLabel(model.settings.downloadRegionID, regions: model.downloadRegions) == "Sweden - Stockholm")
        model.saveSettings(model.settings)
        #expect(commands.map { $0.0 } == ["save_settings"])
        let payload = commands.first?.1["settings"] as? [String: Any]
        #expect((payload?["download_region_id"] as? NSNumber)?.uint32Value == 143)
        model.receive(["event": "settings", "id": "fixture-save_settings", "payload": [
            "settings": model.settings.payload
        ]])
        #expect(model.downloadRegions.count == 1)
        #expect(!model.isSavingSettings)
        #expect(model.settingsNotice == "Настройки сохранены")
    }

    @Test func regionSelectionDoesNotChangeSteamInputEnvironment() {
        let automatic = ClientSettings()
        var regional = automatic
        regional.downloadRegionID = 143
        #expect(automatic != regional)
        #expect(automatic.hasSameRuntimeSettings(as: regional))
        let before = SteamInputEnvironment(client: automatic, game: GameSettings(), crossovers: [], notProton: NotProtonVersion())
        let after = SteamInputEnvironment(client: regional, game: GameSettings(), crossovers: [], notProton: NotProtonVersion())
        #expect(before == after)
        regional.crossoverPath = "/fixture/CrossOver.app"
        #expect(!automatic.hasSameRuntimeSettings(as: regional))
    }
}
