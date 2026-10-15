# macOS: собственный процесс Steam runtime без официального GUI

Проверено: 9 октября 2026, Asia/Shanghai. Исследование исходников и статическая проверка установленной библиотеки; вход в аккаунт, запуск Steam, библиотек или игр не выполнялись.

## Вывод для SkinOs

Есть технически предметный путь: наш Rust-процесс загружает оригинальный Valve `steamclient.dylib` и становится владельцем локального client engine, а наш интерфейс заменяет официальный GUI. Это **не** требует притворяться DRM-free игрой или выдавать несуществующие права на игры. Такой вариант сохраняет собственный игровой backend Valve, но не запускает его основной интерфейс. Доказательство архитектуры существует для Windows/Linux; macOS-hosting ещё необходимо реализовать и проверить.

Отдельный вариант — полностью свой CM-протокол плюс собственная совместимая реализация Steamworks IPC/runtime. Это значительно больший объём. Успешный CM login, наличие файлов игры и подмена SDK-ответов сами по себе не подтверждают полноту игрового runtime.

### Что реально делает OpenSteamClient

Ветка `c#-remake` описывает себя как альтернативный frontend поверх Valve client binary. Заявленные ОС — Windows/Linux, x86_64. Разработчики отдельно отмечают неполную готовность чата, overlay и ряда игровых возможностей. Это первичный пример замены официального UI посредством сохранения оригинального backend, а не доказательство полной совместимости каждой игры. [README, зафиксированный commit](https://github.com/OpenSteamClient/OpenSteamClient/blob/b6703299c982315a0efdf8d6bcea330ab931614b/README.md).

OpenSteamworks различает `ExistingClient` (подключение к работающему Steam) и global instance: приложение становится самим локальным клиентом, к которому подключаются игры. В документации второго режима перечислены зависимости окружения, вспомогательные файлы и конфликт двух одновременно работающих глобальных клиентов. Готового macOS-рецепта там нет. [Документация backend](https://github.com/OpenSteamClient/OpenSteamworks/blob/d0abbe85f4b8314607836e1c588bb48813f642a4/OpenSteamworks/README.md).

### Что подтверждает macos-steam

`Superd22/macos-steam` — другой тип проекта: Windows Steamworks bridge для CrossOver при **работающем нативном Steam**. Его launcher исполняет официальный `steam_osx`; он не заменяет GUI. Использовать этот проект как доказательство уже готового standalone Mac-клиента было бы ошибкой. [README](https://github.com/Superd22/macos-steam/blob/5ad7131bd379a141f8967211520c209b8d089df1/README.md).

Его macOS native probe подтверждает, по отчёту автора, возможность загрузить `steamclient.dylib` из собственного процесса и подключиться к уже работающему Steam как на arm64, так и на x86_64/Rosetta. Проверены live connection и чтение реального достижения. Тот же отчёт показывает конкретное падение из-за несовпадения vtable между версиями интерфейса. Это ценный доказанный транспортный участок, но не тест `CreateGlobalUser` при выключенном Steam. [Native probe findings](https://github.com/Superd22/macos-steam/blob/5ad7131bd379a141f8967211520c209b8d089df1/instruments/native-probe/FINDINGS.md).

## Изученный lifecycle

Ниже описание исходников, **не готовый рецепт ABI для текущего Mac**.

1. Native loader открывает оригинальную библиотеку.
2. Находит C-экспорты `CreateInterface`, `Steam_BGetCallback`, `Steam_FreeLastCallback`.
3. Через factory получает `CLIENTENGINE_INTERFACE_VERSION005`.
4. Backend создаёт global user либо подключается к существующему, в зависимости от явно выбранного режима.
5. Получает внутренние `IClientUser`, `IClientApps`, `IClientAppManager`, `IClientFriends` и другие интерфейсы.
6. Обрабатывает callbacks и asynchronous call results, управляет запуском и состоянием игр.
7. При завершении останавливает callback pump, освобождает user/pipe, вызывает shutdown и освобождает библиотеку.

Шаги загрузки, factory и освобождения отражены в [NativeSteamClient-Statics.cs](https://github.com/OpenSteamClient/OpenSteamworks/blob/d0abbe85f4b8314607836e1c588bb48813f642a4/OpenSteamworks/Native/NativeSteamClient-Statics.cs). Выбор connect/create, получение глобальных handles, настройки UI-process и порядок остановки отражены в [SteamClient.cs](https://github.com/OpenSteamClient/OpenSteamworks/blob/d0abbe85f4b8314607836e1c588bb48813f642a4/OpenSteamworks/SteamClient.cs). Poll/free callback реализован в [NativeSteamClient.cs](https://github.com/OpenSteamClient/OpenSteamworks/blob/d0abbe85f4b8314607836e1c588bb48813f642a4/OpenSteamworks/Native/NativeSteamClient.cs).

У старого C++ OpenSteamClient тот же общий принцип виден буквально в `dlopen`, `dlsym` и `CreateGlobalUser`. Но master помечен deprecated, поэтому его заголовки не следует переносить в новый Mac-проект как актуальные определения ABI. [Старый loader](https://github.com/OpenSteamClient/OpenSteamClient/blob/8d4d18742555959b21453cde0efcd42f336735ee/src/client/ext/steamclient.cpp), [его header](https://github.com/OpenSteamClient/OpenSteamClient/blob/8d4d18742555959b21453cde0efcd42f336735ee/src/client/ext/steamclient.h).

## Ограничения именно Mac-порта

- **Не готовый portable loader.** Текущий OpenSteamworks native loader реализует разбор секций PE/ELF для Windows/Linux; для другой ОС этот путь выбрасывает `NotSupportedException`. Использовать .NET-код без нового Mach-O backend недостаточно. [NativeLibraryEx.cs](https://github.com/OpenSteamClient/OpenSteamworks/blob/d0abbe85f4b8314607836e1c588bb48813f642a4/OpenSteamworks/Native/NativeLibraryEx.cs).
- **ABI должен соответствовать библиотеке.** Сам builder требует точно соответствующую версию библиотеки и зависимостей; неправильные native calls могут завершить процесс. Наличие строки версии factory ещё не проверяет layout всех внутренних интерфейсов. [SteamClientBuilderNative.cs](https://github.com/OpenSteamClient/OpenSteamworks/blob/d0abbe85f4b8314607836e1c588bb48813f642a4/OpenSteamworks/Native/SteamClientBuilderNative.cs).
- **Своя process identity и IPC bootstrap.** Для standalone режима нужно подтвердить обнаружение host-процесса играми, endpoints, ожидания process metadata и расположение изолированных данных macOS. Нельзя перенести Linux PID-file/Steam Runtime инструкции как доказанный Mac-рецепт. Это исследовательская задача SkinOs, а не реализованная возможность.
- **Native игры и Windows/CrossOver — отдельные испытания.** macos-steam демонстрирует in-process Wine unixlib bridge с x86_64 host на проверенных версиях CrossOver. Это полезный будущий адаптер, но увеличивает поверхность совместимости. [ADR unixlib transport](https://github.com/Superd22/macos-steam/blob/5ad7131bd379a141f8967211520c209b8d089df1/docs/adr/0001-unixlib-transport-for-the-bridge.md).
- **Мультиаккаунт.** Несколько сетевых сессий не доказывают возможность нескольких global runtime под одним пользователем macOS. Первую версию host проектировать с одним владельцем игрового runtime и явным переключением его аккаунта. Параллельные global hosts — отдельная проверка IPC и изоляции.

## Фактическая проверка этого Mac

Проверены только установленные binaries и инструменты. Пути с аккаунтами, cookies, токенами и login-конфигурацией не читались.

| Объект | Результат |
|---|---|
| Rust | `rustc 1.98.1`, `cargo 1.98.1` |
| Swift | Apple Swift 6.3.3, target arm64 |
| Xcode | `/Applications/Xcode.app/Contents/Developer` |
| Steam bootstrapper | `/Applications/Steam.app` существует |
| Рабочая библиотека | `~/Library/Application Support/Steam/Steam.AppBundle/Steam/Contents/MacOS/steamclient.dylib` |
| Архитектуры `steamclient.dylib` | x86_64 + arm64 |
| Архитектуры `steam_osx` | x86_64 + arm64 |
| `libtier0_s.dylib`, `libvstdlib_s.dylib` | x86_64 + arm64 |
| Связанные локальные библиотеки | `libtier0_s.dylib`, `libvstdlib_s.dylib`, `libaudio.dylib` |
| Initializer section | `__DATA_CONST,__mod_init_func` присутствует; arm64 размер `0x1140` |

Статически подтверждённые arm64 exports:

```text
CreateInterface
Steam_CreateSteamPipe
Steam_ConnectToGlobalUser
Steam_CreateGlobalUser
Steam_CreateLocalUser
Steam_ReleaseUser
Steam_BReleaseSteamPipe
Steam_BGetCallback
Steam_FreeLastCallback
Steam_GetAPICallResult
Steam_ReleaseThreadLocalMemory
Steam_BConnected
Steam_BLoggedOn
```

Источники этой таблицы — локальные `file`, `nm -arch arm64 -gU`, `otool -L`, `otool -l`, вывод version-команд от 9 октября. Адреса функций намеренно не фиксируются как ABI: они могут измениться при обновлении.

## Первый самостоятельный Rust probe

Первый артефакт должен делать только статическую инвентаризацию: найти установленную библиотеку, получить canonical path, SHA-256, список архитектур, exports каждой архитектуры, зависимости и наличие initializer sections. Для Mach-O используются системные `lipo`, `nm`, `otool`; собственный неполный binary parser не нужен.

Предлагаемый контракт CLI: `skinos-runtime-probe inspect [--runtime PATH] [--json]`.

JSON должен явно сообщать `schema_version`, `inspection_mode: static`, `runtime_initialized: false`, `compatibility: unverified`. Обнаруженные exports — это доступные символы, не подтверждение того, что вход, запуск игры или `CreateGlobalUser` работают.

`dlopen` не включать в этот режим. Apple указывает, что открытие библиотеки также загружает зависимости; её initializer functions выполняются перед возвращением из `dlopen`. Поэтому `RTLD_LOCAL`, `dlsym` без последующего вызова или перенос в дочерний процесс сами по себе не дают гарантию отсутствия записи на диск/сетевой активности. [Apple dynamic library guidelines](https://developer.apple.com/library/archive/documentation/DeveloperTools/Conceptual/DynamicLibraries/100-Articles/DynamicLibraryUsageGuidelines.html), [Apple dlopen manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man3/dlopen.3.html).

Следующий отдельный тест — изолированный disposable runtime-host, затем подтверждение factory, global engine и тестового login. Для этого сначала определить рабочую изоляцию записи и сетевые ограничения, а не считать новый `HOME` или новый процесс достаточной защитой. Это инженерное требование к эксперименту, не заявление, что запуск независимого host невозможен.

## Дальнейший host-контракт SkinOs

Это предлагаемая архитектура:

- GUI общается с Rust supervisor по типизированному IPC и никогда не хранит C++-указатели.
- Runtime adapter живёт в отдельном дочернем процессе; crash/ABI mismatch не обрушает UI.
- Каждое подключение привязано к идентификатору аккаунта, runtime fingerprint и generation ID. Ответы старого host после смены аккаунта отвергаются.
- Состояния `Discovered → Inspected → Loaded → EngineReady → Authenticated → GameRunning → Draining → Stopped` имеют разные критерии готовности; static probe достигает только `Inspected`.
- Адаптер текущей версии проверяет factory, layouts и размеры callback payloads. Неизвестная версия получает статус unsupported/unverified, а не случайный вызов vtable.
- Управление game launch, downloads и Cloud подключается после отдельных интеграционных проверок. Прямой запуск файла игры не засчитывается как Steamworks integration.
- Первая настоящая end-to-end проверка: официальный GUI закрыт, наш host входит в собственный аккаунт, запускает лицензированную native macOS игру, игра получает верный SteamID, callbacks/сохранения работают, после выхода host чисто останавливается.

До последнего теста исследование доказывает технический путь и наличие необходимых компонентов на машине; оно не доказывает полноценную замену Steam на этом Mac.

## Реализованный и проверенный probe

В репозитории создан пакет `skinos-runtime-probe` (Rust workspace). Команды:

```sh
cargo run -p skinos-runtime-probe -- inspect
cargo run -p skinos-runtime-probe -- inspect --json
cargo run -p skinos-runtime-probe -- inspect --runtime '/absolute/path/steamclient.dylib' --json
```

Первый отчёт сохранён в [`outputs/runtime-inspection.json`](../../outputs/runtime-inspection.json). Обе архитектуры, x86_64 и arm64, имеют все 13 проверяемых экспортов, по 23 объявленных зависимости и `__mod_init_func`. SHA-256 исследованного файла: `035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`. Это fingerprint конкретной локальной версии, не общий идентификатор совместимости.

Проверки прошли: `cargo build --workspace`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, успешная генерация/разбор JSON на установленной библиотеке, ненулевой exit code для отсутствующего файла, обычного TOML и каталога. Отчёт явно содержит `runtime_initialized: false` и `compatibility: unverified`.

Probe использует абсолютные пути системных инструментов и передаёт пути отдельными аргументами без shell. SHA-256 повторно проверяется после инспекции, чтобы заметить обновление библиотеки во время чтения. Собственная библиотека не загружается; ни один тест не запускал Steam, игру или процедуру входа.
