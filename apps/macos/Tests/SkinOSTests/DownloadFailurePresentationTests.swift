import Testing
@testable import SkinOS

struct DownloadFailurePresentationTests {
    @Test func accessDeniedSuggestsInspectingAccountWithoutInventingOwnership() {
        let raw = "Account cannot download depot 373307 (Steam result 15)"
        let failure = DownloadFailurePresentation(raw)
        #expect(failure.cause == .accessDenied)
        #expect(failure.action == .inspectAccount)
        #expect(failure.diagnostic == raw)
        for unrecognized in [
            "Account cannot download depot 373307 (Steam result 2)",
            "Account cannot download depot 373307 (Steam result 150)",
            "Other error: \(raw)",
            "\(raw)\nThe cause is different"
        ] {
            let fallback = DownloadFailurePresentation(unrecognized)
            #expect(fallback.cause == .unknown)
            #expect(fallback.action == .inspectDetails)
            #expect(fallback.diagnostic == unrecognized)
        }
    }

    @Test func knownTransportFailuresOfferRetryAndPreserveOriginalDiagnostic() {
        for raw in ["Steam request timed out", "Не удалось получить файлы игры: request timed out"] {
            let failure = DownloadFailurePresentation(raw)
            #expect(failure.cause == .timeout)
            #expect(failure.action == .retry)
            #expect(failure.diagnostic == raw)
        }
        for raw in ["Steam returned no usable HTTPS content servers",
                    "Не удалось получить файлы игры: connection failed",
                    "Не удалось получить файлы игры: content transfer interrupted"] {
            let failure = DownloadFailurePresentation(raw)
            #expect(failure.cause == .connection)
            #expect(failure.action == .retry)
            #expect(failure.diagnostic == raw)
        }
    }

    @Test func diskSpaceAndCoreFailureHaveDifferentRecoveryActions() {
        let space = DownloadFailurePresentation("Недостаточно места: нужно дополнительно 512 МБ, доступно 12 МБ")
        #expect(space.cause == .diskSpace)
        #expect(space.action == .freeSpace)
        let disconnected = DownloadFailurePresentation("Нет соединения с SkinOS Core. Перезапустите приложение.")
        #expect(disconnected.cause == .coreUnavailable)
        #expect(disconnected.action == .restartClient)
    }

    @Test func unknownOrMissingReasonsDoNotInventDiagnosis() {
        for raw in ["Steam result 15", "Лицензия не проверялась", "HTTP 403", "Unexpected private fixture error"] {
            let failure = DownloadFailurePresentation(raw)
            #expect(failure.cause == .unknown)
            #expect(failure.action == .inspectDetails)
            #expect(failure.diagnostic == raw)
        }
        #expect(DownloadFailurePresentation(nil).diagnostic == nil)
        #expect(DownloadFailurePresentation("").diagnostic == nil)
        #expect(DownloadFailurePresentation(nil).cause == .unknown)
    }
}
