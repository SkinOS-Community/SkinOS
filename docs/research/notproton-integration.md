# Встроенный NotProton в SkinOS

Обновлено по первичным исходникам 13 октября 2026. Исходный код NotProton закреплён на [`4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8`](https://github.com/NotProtonNot/NotProton/tree/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8), версия 1.1.7, и хранится без изменений в `third_party/notproton/upstream`. Изменяемые адаптеры SkinOS находятся отдельно в `third_party/notproton/integration`, Rust API — в `crates/compat`.

## Что берём у NotProton

- Подготовку Wine prefix, переносимость настроек, блокировку prefix, размещение Steam bridge и Wine registry из [`compat_run.sh`](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/dylib/feats/compat_run.sh).
- Проверенные по SHA-256 конкретные сборки CrossOver, исходные и изменённые DLL из [`SupportedRunner.swift`](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/app/Sources/NotProtonApp/Model/SupportedRunner.swift). Неизвестная сборка отклоняется до применения смещений патчей.
- Оригинальные upstream `NtdllPatcher`, `RunnerPatcher` и проверку лицензии `CrossOverLicense`, которые компилируются в небольшой консольный helper. Установленное приложение CrossOver не меняется: создаётся отдельная копия под `SkinOs/notproton/runners`.
- `lsteamclient` и `steam.exe` из официального [архива релиза 1.1.7](https://github.com/NotProtonNot/NotProton/releases/tag/v1.1.7). SHA-256 архива сверён с GitHub release API; версия приложения и bundled build-time совпадают с закреплённым commit. Происхождение каждого файла записано в `third_party/notproton/bridge/manifest.json`. Пять Valve DLL, включая `legacycompat/Steam.dll`, повторно проверены по `valve-packages.manifest` этого же upstream commit.

NotProton `lsteamclient` загружает оригинальную `steamclient.dylib` по `STEAM_COMPAT_CLIENT_INSTALL_PATH` ([реализация](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/lsteamclient/unixlib.cpp)). SkinOS передаёт туда свой runtime, свой Mach IPC namespace и адаптер маршрутизации. Оригинальные `steamclient.dll`, `steamclient64.dll` и игровой Steamworks SDK остаются настоящими. Эта интеграция не подменяет ответы проверки лицензии.

Upstream `steam.exe` вызывает настоящий SteamClient, подключается к глобальному пользователю, запускает игру и ждёт её процессов ([steam-shim](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/steam-shim/steam.cpp)). SkinOS вызывает его структурированным argv через Wine после аутентификации собственного runtime. Часть upstream со стартом через LaunchServices и запуском Steam GUI не используется; fallback на оригинальную папку Steam удалён. Параметры пользователя разбираются как аргументы, без shell eval и командных подстановок.

## Шрифты, старые игры и разрешения macOS

Payload содержит 15 готовых Proton fonts, включая `alt/arial.ttf`, и четыре семейные лицензии из официального релиза. Все 19 файлов закреплены по SHA-256 в vendor manifest; updater отклоняет неполный набор. Builder создаёт `fonts-manifest.json` с revision/version и raw `fonts/`. Rust проверяет метаданные и хеши до записи, затем атомарно помещает файлы в собственный `SkinOs/notproton/fonts`. В prefix используются штатные ссылки `link_fonts` upstream; пользовательские обычные файлы шрифтов сохраняются. Отдельно включён `LICENSE.fonts` для build scripts.

Для legacy-игр, включая сценарий исправления Prey, добавлена настоящая Valve `legacycompat/Steam.dll`, сверенная с manifest нового commit. Core подготавливает стабильный `steamapps/common/Steam.dll` → собственный bridge только при отсутствии файла или ссылки. Существующие пользовательские файлы и даже dangling links сохраняются. Upstream `link_legacy_steam_dll` заменён проверкой наличия bridge: sandbox не получает права записи во весь `common`, а ссылка не указывает на временный runtime отдельного запуска.

Создаваемый SkinOS Wine launcher получает `NSMicrophoneUsageDescription`, `NSCameraUsageDescription` и `NSLocalNetworkUsageDescription`. При обновлении существующего Info.plist добавляются эти ключи с сохранением идентичности bundle и сторонних ключей. Helper требует macOS 15+, а для FEX build на macOS 15–25 выбирает входящий Rosetta runner; на 26+ используется FEX.

Upstream `SupportedRunner.swift` хранится без изменений. Из-за ошибки вывода типов при отдельной сборке helper Swift 6.3 builder создаёт compile-копию с явными `[InstalledTool]` типами в цепочке flatMap/filter. Это не меняет таблицы хешей, патчи или выбор сборок. Helper компилируется в Swift 6 mode с deployment target macOS 15.0.

## Настройки

Поддержаны реальные upstream значения `CX_GRAPHICS_BACKEND`: автоматически, `d3dmetal`, `dxmt`, `dxvk`, `wined3d`. `WINEMSYNC=1/0` включает/выключает MSync; автоматический режим сохраняет выбор upstream/prefix. Высокое разрешение соответствует `NOTPROTON_RETINA=1` и реальной настройке Wine registry ([upstream UI](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/dylib/feats/webpatch.c), [runner](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/dylib/feats/compat_run.sh)). ESync не выставляется: для закреплённого пути исполнения его поддержка не подтверждена.

CrossOver и CrossOver Preview обнаруживаются в `/Applications` и `~/Applications`. Выбор игры имеет приоритет над общим выбором. При автоматическом выборе предпочтение отдаётся Preview. Для соответствующей сборки используется существующий `steamapps/compatdata/<appid>`. Другая сборка получает отдельный `SkinOs/notproton/prefixes/<appid>/<build>`: при первом переключении туда копируются и проверяются по SHA-256 пользовательские профили, HKCU registry и данные приложений. Исходный prefix сохраняется, DLL и системный registry другой архитектуры не копируются. Затем у каждой версии собственное состояние и собственный прогресс; повторный выбор не перезаписывает новые сохранения первоначальной копией. Происхождение копии записано в `skinos-prefix-import.json`.

## Проверка SDK

`sdk-probe.exe` (x64) и `sdk-probe32.exe` (x86) собираются системным Clang и Rust LLD. Архитектура выбирается по PE-заголовку исполняемого файла игры. Проверка загружает неизменённую игровую `steam_api64.dll` / `steam_api.dll`, вызывает доступную `SteamAPI_InitFlat` или классическую `SteamAPI_Init`, затем экспортируемый этой версией SteamUser accessor, `BLoggedOn` и `GetSteamID`. Успех требует настоящего подключения и совпадения SteamID с авторизованной сессией; затем выполняется `SteamAPI_Shutdown`.

Для SDK без flat user accessors используется его настоящий SteamClient/pipe/user interface. Только при отсутствии игрового SDK разрешена отдельно маркированная проверка настоящего SteamClient bridge; сломанный игровой SDK не подменяется успешным fallback. Обе probe запускаются через штатный встроенный `steam.exe` bootstrap (`waitforexitandrun`), который создаёт клиентские registry entries в новом prefix. Прямой `runinprefix` до первой игры не обеспечивал эти записи и ломал классическую инициализацию Raft. Подробности и тесты — `third_party/notproton/integration/SDK-PROBE.md`.

## Сборка и обновление

```sh
python3 scripts/build-notproton.py
cargo test -p skinos-compat
cargo clippy -p skinos-compat --all-targets -- -D warnings
```

Сборка готовит `target/notproton`: runner, helper, Windows SDK probe, ресурсы, bridge, метаданные и лицензии. Финальная упаковка копирует этот каталог в `SkinOS.app/Contents/Resources/notproton`. После упаковки установленное приложение NotProton больше не является зависимостью запуска; CrossOver берётся из выбранного установленного приложения только для собственной подготовленной копии.

Bridge упакован в gzip-ресурсы и восстанавливается побайтно с проверкой хеша: подпись x86 Unix `lsteamclient.so` ломает его загрузку Wine, что прямо отмечено в upstream `lsteamclient/build.sh`. Подписывается собственный native helper и внешний app bundle, готовые Wine bridge файлы не переподписываются.

Для обновления выбрать точный upstream commit и соответствующий официальный release asset либо собрать его bridge (`make bridge`, `make app-payload`). Файлы раскладываются по layout `BridgePayload.stage`; Valve DLL сверяются с manifest выбранного commit. Можно приложить JSON происхождения с repository/revision/version и SHA-256 каждого файла:

```sh
python3 scripts/update-notproton.py --revision <полный-commit> --bridge-source <каталог-нового-bridge> --bridge-provenance <provenance.json>
```

Скрипт обновляет pin, заново компилирует upstream patcher, применяет небольшой адаптер к runner, проверяет shell syntax, Rust tests и Clippy. Он не позволяет молча использовать прежние бинарники для нового commit. Source, bridge pin, метаданные и готовый payload публикуются одной транзакцией. Прежние четыре компонента сохраняются в `target/notproton-update-backups`; сбой публикации откатывает их. Неверный revision либо SHA-256 в provenance отклоняется до публикации. После обновления обязательна сквозная проверка настоящего SDK и Soundpad через SkinOS. Обновление не изменяет запущенные игры и пользовательские prefix.

Включены оригинальные LICENSE, LICENSE.lsteamclient, LICENSE.steam-shim и NOTICE; условия различаются по компонентам, см. [upstream NOTICE](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/NOTICE). Сам CrossOver в пакет SkinOS не копируется.

## Обновление 1.1.7 от 13 октября 2026

- [Commit](https://github.com/NotProtonNot/NotProton/commit/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8): `4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8`.
- [Официальный архив](https://github.com/NotProtonNot/NotProton/releases/download/v1.1.7/NotProton.zip): GitHub asset ID `631548894`, SHA-256 `df116f5185fb57b6f6f8ff5f593b9851edb697a01777e403e1fd23185f9bdbff`. Версия приложения 1.1.7; bundled build-time совпал с commit-time `1791743477`.
- [Diff upstream 1.1.3→1.1.7](https://github.com/NotProtonNot/NotProton/compare/859359449fbc2f8b544b363518dd9ae9f52d0908...4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8): шрифты Proton, поддержка Sequoia/Rosetta, legacy Steam.dll linkage, usage descriptions и более стабильное обновление launcher. Изменения patcher оригинального Steam не исполняются: SkinOS по-прежнему использует собственный runtime и launcher.
- Проверены 12 bridge-файлов и 19 font/license-файлов (93 323 173 байта). Происхождение каждого записано в vendor manifest; Valve dependencies сверены с новым upstream manifest.
- Пройдены 22 Python-теста updater/provenance/fonts/SDK/host boundaries, 14 Rust compat-тестов и строгий Clippy. Изолированные проверки сгенерированного runner подтвердили стандартные/альтернативные шрифты, сохранение обычного пользовательского font-файла и отсутствие перезаписи чужой Steam.dll ссылки. Helper Mach-O имеет minimum OS 15.0.
- Backup source/bridge/metadata/payload: `target/notproton-update-backups/1791879537440364000-859359449fbc`. Краткий отчёт: `outputs/notproton-update-1.1.7.json`. Сборку приложения и проверку игры выполняет основная задача; updater не запускал пользовательские runtime, prefix, CrossOver или Steam.

## Обновление 1.1.3 от 10 октября 2026

- [Commit](https://github.com/NotProtonNot/NotProton/commit/859359449fbc2f8b544b363518dd9ae9f52d0908): `859359449fbc2f8b544b363518dd9ae9f52d0908`.
- [Официальный архив](https://github.com/NotProtonNot/NotProton/releases/download/v1.1.3/NotProton.zip): GitHub asset ID `623878102`, SHA-256 `9628b069f481457292b914319b9d7d57420e03b39ed7f441cf29d5ff34b7acc8`. Bundled build-time и время commit: `1791518587`.
- По [diff upstream](https://github.com/NotProtonNot/NotProton/compare/bd0fcaa22e0ea28ac67d861fc1c4e7611207f696...859359449fbc2f8b544b363518dd9ae9f52d0908), runtime получает `CX_APPLEGPTK_LIBD3DSHARED_PATH`, необходимый Rosetta в CrossOver 26.3; таблица поддержанных runner хешей дополнена сборкой crossoverchina.com. Исходники Steam shim/lsteamclient относительно прежнего pin не менялись; бинарники заменены официальными артефактами нового релиза.
- Пройден upstream `envcheck.sh` и отдельная проверка наследования GPTK path в сгенерированном runner SkinOS, включая путь с пробелами. Сохранены обязательные пути собственного runtime/loader и прямой foreground запуск; fallback к Steam GUI отсутствует.
- Пройдены 9 тестов pin/provenance/rollback, 8 SDK probe tests, 7 Rust compat tests и строгий Clippy. Собраны новый helper и обе Windows probe. Это проверки сборки и изолированных fixtures; живую проверку готового клиента выполняет основная задача.
- Предыдущая версия сохранена в `target/notproton-update-backups/1791562515514434000-bd0fcaa22e0e`. Краткий отчёт — `outputs/notproton-update-1.1.3.json`.

## Исторические локальные проверки первой интеграции (1.1.0)

- Создана собственная копия CrossOver Preview `27.0.0.41069-fex`, лицензия проверена upstream-кодом, ntdll и bridge установлены и проверены по SHA-256.
- Подготовлен план для существующего Soundpad 629520 без запуска Wine и без изменения оригинального CrossOver; результат — `outputs/soundpad-compat-plan.json`.
- Собран Windows SDK probe, три теста Rust и строгий Clippy пройдены; отдельный тест проверяет сохранность данных и повторное переключение между prefix.
- Фактический Windows SDK тест через собственный runtime прошёл: InitFlat=0, пользователь онлайн, SteamID совпал. Soundpad запущен, исходный Steam не запущен (`probe-37346-1791493985271286000`, отчёт основной проверки SkinOS).
- Устранена конкретная причина падения FEX под sandbox: Wine создаёт native обёртки в системном пользовательском temp-каталоге `winetemp-<inode>-<size>-<mtime-sec>-<mtime-nsec>`. План разрешает выполнение только в каталоге, рассчитанном из выбранного loader. Дифференциальная проверка сохранила все остальные ограничения и запрет запуска `/usr/bin/open`; разрешение точного пути позволило настоящей Windows программе выполнить код. Весь temp-каталог не разрешается на исполнение.
