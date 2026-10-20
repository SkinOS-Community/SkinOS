import Foundation

struct SteamAccount: Identifiable {
    let id: String
    let accountName: String
    let displayName: String
    let avatarURL: URL?
}

struct SteamGame: Identifiable, Equatable {
    var id: Int { appID }
    let appID: Int
    let name: String
    let playtimeMinutes: Int
    var installedDirectory: String?
    var installedSizeBytes: Int64?
    let supportsMacOS: Bool
    let supportsWindows: Bool
    let supportsLinux: Bool
    let lastPlayed: Int64
    let headerURL: URL?
    var updateAvailable: Bool
    var installedBuildID: String?
    var availableBuildID: String?
    var installedPlatform: String?

    init?(json: [String: Any]) {
        guard let appID = json["appid"] as? Int, let name = json["name"] as? String else { return nil }
        self.appID = appID
        self.name = name
        playtimeMinutes = json["playtime_minutes"] as? Int ?? 0
        installedDirectory = json["installed_dir"] as? String
        installedSizeBytes = (json["installed_size_bytes"] as? NSNumber)?.int64Value
        supportsMacOS = json["macos_supported"] as? Bool ?? false
        supportsWindows = json["windows_supported"] as? Bool ?? false
        supportsLinux = json["linux_supported"] as? Bool ?? false
        lastPlayed = (json["last_played"] as? NSNumber)?.int64Value ?? 0
        updateAvailable = json["update_available"] as? Bool ?? false
        installedBuildID = json["installed_build_id"] as? String
        availableBuildID = json["available_build_id"] as? String
        installedPlatform = json["installed_platform"] as? String
        headerURL = URL(string: json["header_url"] as? String
            ?? "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/\(appID)/header.jpg")
    }

    var isInstalled: Bool { !(installedDirectory ?? "").isEmpty }
    var playtime: String {
        guard playtimeMinutes > 0 else { return "Ещё не играли" }
        return String(format: "%.1f ч в игре", Double(playtimeMinutes) / 60)
    }
}

struct GameDownload: Identifiable {
    let id: Int
    var phase = "preparing"
    var progress: Double?
    var downloadedBytes: Int64 = 0
    var totalBytes: Int64 = 0
    var bytesPerSecond: Double = 0
    var etaSeconds: Int?
    var reusedBytes: Int64 = 0
    var processedBytes: Int64 = 0
    var installTotalBytes: Int64 = 0
    var filesCompleted: Int = 0
    var totalFiles: Int = 0
    var chunksCompleted: Int = 0
    var totalChunks: Int = 0
    var queuePosition: Int = 0
    var message: String?
    var complete = false
    var failed = false
    var isUpdate = false
    var platform: InstallPlatform?

    var cancelled: Bool { phase == "cancelled" }
    var isActive: Bool { !complete && !failed && !cancelled }
    var isRunning: Bool { ["preparing", "verifying", "downloading", "installing", "pausing", "cancelling"].contains(phase) && isActive }
    var canPause: Bool { ["queued", "preparing", "verifying", "downloading", "installing"].contains(phase) && isActive }
    var canResume: Bool { phase == "paused" && isActive }
    var canCancel: Bool { isActive && !["cancelling", "pausing"].contains(phase) }
    var canRetry: Bool { failed || cancelled }
    var isNetworkPhase: Bool { ["downloading", "download", "chunks"].contains(phase) && isActive }
    var isWaitingOrTerminal: Bool { ["queued", "paused", "pausing", "cancelling", "cancelled", "failed"].contains(phase) }
    var isDiskPhase: Bool { ["verifying", "verification", "checking", "installing", "finalizing", "committing"].contains(phase) }
    var displayProgress: Double? {
        if complete { return 1 }
        // Pausing a disk verification preserves its phase progress in the core.
        // Once inactive, pair the percentage with the byte counters actually shown.
        if isWaitingOrTerminal {
            if totalBytes > 0 { return min(1, max(0, Double(downloadedBytes) / Double(totalBytes))) }
            if installTotalBytes > 0 { return min(1, max(0, Double(processedBytes) / Double(installTotalBytes))) }
            return nil
        }
        if isDiskPhase, installTotalBytes > 0 { return min(1, max(0, Double(processedBytes) / Double(installTotalBytes))) }
        if isDiskPhase, totalFiles > 0 { return min(1, max(0, Double(filesCompleted) / Double(totalFiles))) }
        guard let progress, progress.isFinite else { return nil }
        return min(1, max(0, progress))
    }
    var byteDescription: String {
        if isDiskPhase {
            if installTotalBytes > 0 { return "\(phase == "verifying" ? "Проверено" : "Обработано") \(Self.bytes(processedBytes)) из \(Self.bytes(installTotalBytes))" }
            return "Проверяем данные на диске…"
        }
        if totalBytes > 0 {
            return "\(isWaitingOrTerminal ? "Скачано: " : "")\(Self.bytes(downloadedBytes)) из \(Self.bytes(totalBytes))"
        }
        if isWaitingOrTerminal, installTotalBytes > 0 { return "Обработано: \(Self.bytes(processedBytes)) из \(Self.bytes(installTotalBytes))" }
        if installTotalBytes > 0 { return "Размер установки: \(Self.bytes(installTotalBytes))" }
        return phase == "queued" ? "Размер будет определён при подготовке" : "Определяем размер загрузки…"
    }
    var speedDescription: String {
        guard bytesPerSecond > 0, bytesPerSecond.isFinite else { return "Скорость пока неизвестна" }
        return "\(Self.bytes(Int64(min(bytesPerSecond, Double(Int64.max - 1024)))))/с"
    }
    var etaDescription: String {
        guard let seconds = etaSeconds, seconds >= 0 else { return "Рассчитываем время…" }
        if seconds < 60 { return "Осталось меньше минуты" }
        if seconds < 3_600 { return "Осталось ~\((seconds + 59) / 60) мин" }
        let hours = seconds / 3_600
        let minutes = (seconds % 3_600) / 60
        return minutes == 0 ? "Осталось ~\(hours) ч" : "Осталось ~\(hours) ч \(minutes) мин"
    }
    var phaseDescription: String {
        switch phase.lowercased() {
        case "queued": return "В очереди"
        case "preparing", "metadata", "authorizing", "manifest", "manifests": return "Подготовка загрузки"
        case "downloading", "download", "chunks": return isUpdate ? "Загрузка обновления" : "Загрузка"
        case "verifying", "verification", "checking": return "Проверка файлов на диске"
        case "installing", "finalizing", "committing": return "Установка"
        case "pausing": return "Приостанавливаем…"
        case "paused": return "Приостановлено"
        case "cancelling": return "Отменяем…"
        case "cancelled": return "Загрузка отменена"
        case "failed": return "Ошибка загрузки"
        case "complete", "completed", "done": return "Готово"
        default: return phase
        }
    }
    static func bytes(_ value: Int64) -> String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return formatter.string(fromByteCount: max(0, value))
    }
}

struct ClientSettings: Equatable {
    var steamappsDirectory = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Application Support/Steam/steamapps").path
    var crossoverPath = ""
    var downloadRegionID: UInt32?

    init() {}
    init(json: [String: Any]) {
        self.init()
        steamappsDirectory = json["steamapps_dir"] as? String ?? steamappsDirectory
        crossoverPath = json["crossover_path"] as? String ?? ""
        downloadRegionID = DownloadRegion.parseID(json["download_region_id"])
    }
    var payload: [String: Any] {
        ["steamapps_dir": steamappsDirectory, "crossover_path": crossoverPath.isEmpty ? NSNull() : crossoverPath as Any,
         "download_region_id": downloadRegionID.map { NSNumber(value: $0) } ?? NSNull()]
    }
    func hasSameRuntimeSettings(as other: ClientSettings) -> Bool {
        steamappsDirectory == other.steamappsDirectory && crossoverPath == other.crossoverPath
    }
}

struct CrossOverInstallation: Identifiable, Equatable {
    var id: String { path }
    let path: String
    let name: String
    let version: String
    let preview: Bool

    init?(json: [String: Any]) {
        guard let path = json["path"] as? String else { return nil }
        self.path = path
        preview = json["preview"] as? Bool ?? false
        name = json["name"] as? String ?? (preview ? "CrossOver Preview" : "CrossOver")
        version = json["version"] as? String ?? ""
    }
    var label: String { version.isEmpty ? name : "\(name) · \(version)" }
}

struct NotProtonVersion: Equatable {
    var version: String = ""
    var revision: String = ""
    init() {}
    init(json: [String: Any]) {
        version = json["version"] as? String ?? ""
        revision = json["revision"] as? String ?? ""
    }
    var label: String {
        if !version.isEmpty { return version }
        return revision.isEmpty ? "Встроен в SkinOS" : String(revision.prefix(8))
    }
}

struct GameSettings: Equatable {
    var compatibility = "auto"
    var graphics = "auto"
    var sync = "auto"
    var highResolution = false
    var steamInput = false
    var launchArguments = ""
    var crossoverPath = ""
    init() {}
    init(json: [String: Any]) {
        compatibility = json["compatibility"] as? String ?? "auto"
        graphics = json["graphics"] as? String ?? "auto"
        sync = json["sync"] as? String ?? "auto"
        highResolution = json["high_resolution"] as? Bool ?? false
        steamInput = json["steam_input"] as? Bool ?? false
        launchArguments = json["launch_arguments"] as? String ?? ""
        crossoverPath = json["crossover_path"] as? String ?? ""
    }
    var payload: [String: Any] {
        ["compatibility": compatibility, "graphics": graphics, "sync": sync,
         "high_resolution": highResolution, "steam_input": steamInput, "launch_arguments": launchArguments,
         "crossover_path": crossoverPath.isEmpty ? NSNull() : crossoverPath as Any]
    }
}

enum AppSection: String, CaseIterable, Identifiable {
    case library = "Библиотека"
    case store = "Магазин"
    case friends = "Друзья и чат"
    case profile = "Профиль"
    case notifications = "Уведомления"
    case downloads = "Загрузки"
    case account = "Аккаунт"
    case settings = "Настройки"
    var id: Self { self }
    var symbol: String {
        switch self {
        case .library: return "square.grid.2x2"
        case .store: return "bag"
        case .friends: return "bubble.left.and.bubble.right"
        case .profile: return "person.crop.rectangle"
        case .notifications: return "bell"
        case .downloads: return "arrow.down.circle"
        case .account: return "person.crop.circle"
        case .settings: return "slider.horizontal.3"
        }
    }
}

enum GameFilter: String, CaseIterable, Identifiable {
    case all = "Все игры"
    case mac = "Для Mac"
    case windows = "Windows"
    case linux = "Linux"
    var id: Self { self }
}


enum InstallFilter: String, CaseIterable, Identifiable {
    case all = "Любая установка", installed = "Установлены", uninstalled = "Не установлены", updates = "Есть обновление"
    var id: Self { self }
}
enum PlayFilter: String, CaseIterable, Identifiable {
    case all = "Любое время игры", played = "Уже играли", unplayed = "Ещё не играли", favorites = "Избранное"
    var id: Self { self }
}
enum LibrarySort: String, CaseIterable, Identifiable {
    case name = "По названию", recent = "Недавно играли", playtime = "По времени в игре"
    var id: Self { self }
}
