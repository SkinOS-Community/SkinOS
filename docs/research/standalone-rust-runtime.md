# Независимый Steam-клиент на Rust: проверка реального механизма запуска

Проверено 9 октября 2026 года. Анализ исходного кода в read-only клонах; сторонние программы не запускались, вход в Steam не выполнялся, файлы Steam и секреты пользователя не читались. Наличие реализации ниже означает найденный путь в коде, а не подтверждённый запуск игры в этой сессии.

## Основной вывод

Существуют клиенты, которые заменяют официальный интерфейс Steam и сами загружают библиотеки Valve. Это самостоятельный путь, отличный от скрытого запуска всего официального приложения. **OpenSteamClient реализует именно такую архитектуру:** загружает `steamclient.so`, создаёт внутренний `IClientEngine`, получает интерфейсы аккаунта, библиотеки и запуска игр. Поэтому утверждение, что единственный вариант — держать официальный Steam целиком в фоне, слишком сильное. [Загрузка библиотеки и интерфейсов](https://github.com/OpenSteamClient/OpenSteamClient/blob/8d4d18742555959b21453cde0efcd42f336735ee/src/client/ext/steamclient.cpp), [вызов LaunchApp](https://github.com/OpenSteamClient/OpenSteamClient/blob/8d4d18742555959b21453cde0efcd42f336735ee/src/client/interop/app.cpp#L425-L436).

При этом проверенные Rust-проекты не дают готовый универсальный Steamworks runtime для macOS. Они решают CM-сеть, авторизацию, метаданные, чат и загрузку контента; интеграция с уже собранными играми остаётся отдельным слоем. Для SkinOs наиболее перспективна комбинация **Rust для собственных сервисов + отдельный процесс, хостящий совместимые библиотеки Valve**, с проверкой macOS ABI, IPC и реальных игр. Это инженерная рекомендация по результатам анализа, не подтверждение работоспособности будущего порта.

## Зафиксированные версии

| Проект | Исследованная версия | Что реально проверено |
|---|---|---|
| Aurelia | `50c44d33b97f4aee6fe694e90c464951970d9fd8`, Cargo version `0.1.37` | Launch pipeline, Steam modes, depot download, Cloud, macOS filters |
| OpenSteamClient, C++ master | `8d4d18742555959b21453cde0efcd42f336735ee` | Loader, updater, launch call, Linux service |
| OpenSteamClient, C# remake | `b6703299c982315a0efdf8d6bcea330ab931614b` | macOS specifics, install detection, service startup, declared release blockers |
| steam-cm-protocol | `b8990de242f61d3ee2a15d80ad051d8c3e1222b8` | Public commands, auth, connection/license state |
| steam-vent | tag `v0.5.0`, `1325a4a0235a22c3604ea943b4362148afc13276` | Module surface, dependency metadata, auth-ticket example |
| steam-client-rs | `bcc8404407e06253fcd8472ed65dd1c784f7e06c` | App-auth ticket implementation and incomplete acknowledgement path |

## Aurelia: что означает «без Steam»

### Три режима запуска

`SteamMode` определяет три разных режима. `HostBridge` соединяет игру с работающим Steam хоста. `InWineRuntime` использует настоящие `steamclient.dll` и `steam.exe`, установленные в Wine-префикс. `Standalone` не предоставляет Steam DRM. Следовательно, «не установлен Steam хоста» и «не работает никакой официальный Steam» — разные утверждения; второй режим удовлетворяет первому, но не второму. [Определение режимов и их выбор](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/infra/runners/wine_tkg.rs#L995-L1074).

Перед standalone-запуском Aurelia запрашивает ownership ticket, однако это только диагностическая проверка: результат успешного запроса отбрасывается, ошибка и таймаут не останавливают запуск. В этом пути нет передачи ticket в полноценную реализацию локального Steamworks API. Получить подтверждение владения через сеть недостаточно, чтобы удовлетворить все вызовы игры в `ISteamUser`, callbacks, matchmaking, networking и storage. Последнее — вывод из исследованного пути, а не утверждение, что получение ticket технически бесполезно. [Preflight запуска](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/commands/launch.rs#L132-L158), [запрос ticket](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/steam_client/client.rs#L99-L116).

Семейные игры автоматически требуют Steam integration. Игры, установленные внутри отдельного Wine Steam, передаются непосредственно этому Steam; настройки Proton/DXVK и учёт игровой сессии Aurelia для такого запуска не действуют. [Основной launch path](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/steam_client/launch.rs#L77-L112), [ветка Wine Steam](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/commands/launch.rs#L43-L84).

**Ответ на главный вопрос:** Aurelia не подтверждает универсальный самостоятельный online-запуск лицензионных Steamworks-игр без официального runtime. Её самостоятельный путь применим к играм, которым не нужен работающий Steam runtime; режим совместимости задействует настоящий Steam. Это не просто пересказ README, а устройство исследованного launch pipeline.

### macOS: сборка CLI есть, запуск native-игр отсутствует

Workflow содержит отдельные macOS Intel и ARM runners. Однако parser launch options явно пропускает записи `oslist=macos`; `DepotPlatform` содержит только Linux/Windows, и фильтры исключают Mac-only depots. Это существенное расхождение между наличием бинарника CLI для Mac и функциональностью игрового backend для Mac. [Release workflow](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/.github/workflows/release.yml#L46-L51), [исключение Mac launch](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/steam_client.rs#L1362-L1388), [платформы depot](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/core/models.rs#L239-L243), [фильтры depot](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/steam_client.rs#L1270-L1301).

Нужно добавить отдельные Mac-target и Mach-O архитектуры, корректный выбор depot/launch option, запуск `.app` и бинарников, управление Rosetta там, где необходимо, Mac Cloud roots и соответствующий runtime-host. Это список работ, выведенный из обнаруженных ограничений; ни один из пунктов не проверялся игровым запуском.

### Что у Aurelia действительно полезно

| Подсистема | Наблюдение по исходникам | Значение для SkinOs |
|---|---|---|
| CM и auth | Зависит от `steam-vent` 0.5 и protobuf-сообщений | Пример Rust-клиента, работающего независимо от Steam GUI |
| Downloads | Запрашивает CDN hosts, depot key, manifest request code и CDN token; собирает скачанный контент | Полезный референс собственного загрузчика с настоящей авторизацией |
| Cloud | Enumerate, download, begin upload, HTTP transfer, commit upload; baseline и обнаружение конфликтов | Реальная основа файловой синхронизации, но не полная замена in-game RemoteStorage API |
| Cloud paths | Non-Windows ветка поддерживает Linux и Wine roots, без Mac roots | Нужна отдельная macOS реализация |
| Магазин/кошелёк | Получает web-scoped access token и читает web endpoints; market-модуль объявлен read-only | Референс связывания CM и веб-сессии; покупки этим не доказаны |
| License | GPL-3.0, `steam-vent` MIT | Прямое использование кода Aurelia следует оценивать отдельно от использования MIT-сетевой библиотеки |

Источники: [Cargo](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/Cargo.toml), [download pipeline](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/steam_client/install.rs#L480-L585), [Cloud](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/library/cloud_sync.rs), [web session](https://github.com/Drackrath/Aurelia/blob/50c44d33b97f4aee6fe694e90c464951970d9fd8/src/steam_client/market.rs#L1-L138).

## OpenSteamClient: полезный архитектурный предшественник

C++-клиент делает `dlopen` библиотеки Valve, получает `CreateInterface`, создаёт engine, global user и pipe, затем получает внутренние интерфейсы. Запуск выполняет `IClientAppManager::LaunchApp`, установка — `InstallApp`. Updater скачивает нужные клиентские компоненты с Valve CDN и выбирает файлы по allowlist: библиотеки клиента, services, overlay и вспомогательные программы. Это полноценнее сетевой библиотеки и не требует собственного переписывания каждого Steamworks-интерфейса. [Loader](https://github.com/OpenSteamClient/OpenSteamClient/blob/8d4d18742555959b21453cde0efcd42f336735ee/src/client/ext/steamclient.cpp), [launch/install](https://github.com/OpenSteamClient/OpenSteamClient/blob/8d4d18742555959b21453cde0efcd42f336735ee/src/client/interop/app.cpp#L425-L445), [компоненты updater](https://github.com/OpenSteamClient/OpenSteamClient/blob/8d4d18742555959b21453cde0efcd42f336735ee/src/client/startup/updater.h).

Старый C++ master объявлен deprecated/unsupported; в его build configuration service собирается 32-битным. Это Linux-референс, не портируемая целиком Mac-реализация. [Статус master](https://github.com/OpenSteamClient/OpenSteamClient/blob/8d4d18742555959b21453cde0efcd42f336735ee/README.md), [build configuration](https://github.com/OpenSteamClient/OpenSteamClient/blob/8d4d18742555959b21453cde0efcd42f336735ee/CMakeLists.txt#L196-L207).

В C# remake есть `MacOSSpecifics` со значениями `steamclient.dylib` и `steam_client_osx`, а проект перечисляет `osx-x64` среди runtime identifiers. Но install detection имеет TODO для Mac, а запуск SteamService на Mac попадает в неподдерживаемую ветку. Присутствие имён платформы не доказывает рабочий backend. [Mac-specific names](https://github.com/OpenSteamClient/OpenSteamClient/blob/b6703299c982315a0efdf8d6bcea330ab931614b/OpenSteamworks.Client/Utils/OSSpecific/MacOSSpecifics.cs), [install detection](https://github.com/OpenSteamClient/OpenSteamClient/blob/b6703299c982315a0efdf8d6bcea330ab931614b/OpenSteamworks.Client/Managers/InstallManager.cs), [service startup](https://github.com/OpenSteamClient/OpenSteamClient/blob/b6703299c982315a0efdf8d6bcea330ab931614b/OpenSteamworks.Client/Startup/SteamService.cs).

README remake прямо описывает alpha-состояние, неполный чат/overlay/achievements, отсутствие UI разрешения Cloud-конфликтов и поддержку Windows/Arch x86_64. Его нельзя использовать как доказательство полного покрытия Steam. При этом это самый полезный из исследованных примеров **самостоятельного хоста библиотек Valve**. [Статус и ограничения remake](https://github.com/OpenSteamClient/OpenSteamClient/blob/b6703299c982315a0efdf8d6bcea330ab931614b/README.md).

## Rust-протоколы: что брать для собственного ядра

### steam-cm-protocol

MIT-библиотека, напрямую соединяющаяся с Steam CM по WebSocket. Public `RunCommand` включает presence, friend data, library, achievements, send/typing/history chat. В исследованной структуре нет локального Steamworks runtime/IPC сервера, downloader или game launcher. Она хорошо соответствует первой социальной и библиотечной части SkinOs, но не заменяет игровую интеграцию. [Публичные команды и auth](https://github.com/LargeModGames/steam-cm-protocol/blob/b8990de242f61d3ee2a15d80ad051d8c3e1222b8/src/client.rs#L1-L145), [модули](https://github.com/LargeModGames/steam-cm-protocol/blob/b8990de242f61d3ee2a15d80ad051d8c3e1222b8/src/lib.rs).

### steam-vent

MIT, Rust 1.88+, на исследованном tag 0.5.0. Даёт CM connection, authentication, raw messages, typed service calls и GameCoordinator. В экспортах нет реализации локальных ABI/IPC для готовой игры. Пример `auth_ticket` получает game-connect tokens из сетевых сообщений; это отдельный кирпич, не готовая замена runtime. Для более широкого клиента кандидат предпочтительнее узкой библиотеки, если команда готова писать высокоуровневые сервисы. Последнее — архитектурная оценка. [Cargo](https://codeberg.org/steam-vent/steam-vent/src/commit/1325a4a0235a22c3604ea943b4362148afc13276/Cargo.toml), [модули](https://codeberg.org/steam-vent/steam-vent/src/commit/1325a4a0235a22c3604ea943b4362148afc13276/src/lib.rs), [ticket example](https://codeberg.org/steam-vent/steam-vent/src/commit/1325a4a0235a22c3604ea943b4362148afc13276/examples/auth_ticket.rs).

### steam-client-rs

Ещё один Rust CM-кандидат с большим набором services. Наличие `create_auth_session_ticket` выглядит ближе к игровому auth, но `activate_auth_session_tickets` в исследованной версии после отправки ClientAuthList сразу возвращает пустой список результатов; комментарий признаёт отсутствие ожидания ответа. Поэтому этот код не является доказательством успешно проверенного online-auth, и не следует считать завершённым по имени API. [App auth implementation](https://github.com/lengoclongk59/steam-client-rs/blob/bcc8404407e06253fcd8472ed65dd1c784f7e06c/src/services/appauth.rs#L91-L234).

## Конкретный runtime-spike для SkinOs

Предлагаемая последовательность — вывод из анализа, а не описание уже готовой функции:

1. Собрать read-only инвентаризацию имеющихся Mac-компонентов Steam: архитектуры, подписи, exports, зависимости, пути и версии. Не менять существующую установку.
2. Создать отдельный минимальный процесс runtime-host, который загружает библиотеку из явно выбранного источника и проверяет только создание/закрытие interfaces. Не смешивать нестабильный private ABI с GUI-процессом.
3. Проверить вход, SteamID, реальные лицензии, callbacks и reconnect без запуска официального frontend.
4. Запустить собственный Steamworks diagnostic harness с правомерным AppID/доступом либо подходящую принадлежащую пользователю тестовую игру. Проверить фактический SteamID, logged-on state, callback delivery и отсутствие полного официального приложения среди процессов.
5. В отдельном наборе игр проверить native ARM/Intel, online ticket, lobby/invite, achievements и save lifecycle. Успех DRM-free игры не закрывает Steamworks-проверку.
6. Проверить два аккаунта последовательно: отключение первого runtime, очистка временных handoff-состояний, запуск второго с проверенным SteamID. Многопрофильный чат и несколько одновременных игровых runtime не считать одной и той же задачей.
7. Измерить весь process tree, latency и память. Если host-библиотеки подтягивают большой дополнительный процесс, включить его в измерения.

В результате spike должен дать конкретный список совместимых Mac-библиотек, воспроизводимую инструкцию, диагностический отчёт по настоящим играм и перечень ещё не реализованных интерфейсов. До этого «все Steam-функции без Steam.app» — целевая спецификация, а не проверенное свойство программы.

## Что исследование не установило

- В этой ветке исследования не проверялись takedown-заявления Valve и юридический статус конкретных проектов; наличие доступного GitHub-репозитория не доказывает согласие Valve.
- Не выполнялись покупки, вход в аккаунты, загрузки платных depot или модификации установленных игр.
- Не найдена и не проверена готовая Rust-реализация, которая на Mac полностью заменяет локальный Steamworks runtime для любых коммерческих игр, сохраняя весь online-функционал. Это результат ограниченного целевого поиска, а не доказательство невозможности такой реализации.
