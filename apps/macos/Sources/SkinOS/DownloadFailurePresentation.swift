import Foundation

struct DownloadFailurePresentation: Equatable {
    enum Cause: Equatable { case accessDenied, timeout, connection, diskSpace, coreUnavailable, unknown }
    enum Action: Equatable { case inspectAccount, retry, freeSpace, restartClient, inspectDetails }

    let cause: Cause
    let action: Action
    let title: String
    let guidance: String
    let diagnostic: String?

    init(_ message: String?) {
        diagnostic = message.flatMap { $0.isEmpty ? nil : $0 }
        let raw = message?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        // Match the downloader's complete messages, not arbitrary substrings in an error chain.
        if raw.range(of: #"^Account cannot download depot [0-9]+ \(Steam result 15\)$"#,
                     options: .regularExpression) != nil {
            cause = .accessDenied
            action = .inspectAccount
            title = "Steam отказал в доступе к файлам игры."
            guidance = "Проверьте, какой аккаунт выбран в SkinOS, и доступен ли ему выбранный контент. По этому ответу нельзя определить точную причину отказа."
        } else if ["Steam request timed out", "Не удалось получить файлы игры: request timed out"].contains(raw) {
            cause = .timeout
            action = .retry
            title = "Сервер Steam не ответил вовремя."
            guidance = "Проверьте подключение к интернету и повторите загрузку."
        } else if ["Не удалось получить файлы игры: connection failed",
                   "Не удалось получить файлы игры: content transfer interrupted",
                   "Не удалось получить файлы игры: content server unavailable",
                   "Steam returned no usable HTTPS content servers"].contains(raw) {
            cause = .connection
            action = .retry
            title = "Не удалось получить файлы с сервера загрузки."
            guidance = "Проверьте подключение к интернету и повторите загрузку. Регион загрузки можно изменить в настройках."
        } else if raw.range(of: #"^Недостаточно места: нужно дополнительно [0-9]+ МБ, доступно [0-9]+ МБ$"#,
                            options: .regularExpression) != nil {
            cause = .diskSpace
            action = .freeSpace
            title = "Для установки не хватает места на диске."
            guidance = "Освободите место на диске выбранной библиотеки и повторите загрузку. Требуемый объём указан в подробностях."
        } else if raw == "Нет соединения с SkinOS Core. Перезапустите приложение."
                    || raw == "Соединение прервано. Повторите загрузку после перезапуска." {
            cause = .coreUnavailable
            action = .restartClient
            title = "Соединение с загрузчиком прервано."
            guidance = "Перезапустите SkinOS, затем обновите очередь, чтобы проверить состояние загрузок."
        } else {
            cause = .unknown
            action = .inspectDetails
            title = "Не удалось выполнить операцию загрузки."
            guidance = diagnostic == nil
                ? "Загрузчик не передал подробностей. Обновите очередь и проверьте состояние загрузки."
                : "Причина не распознана. Откройте технические подробности; их можно скопировать для сообщения об ошибке."
        }
    }
}
