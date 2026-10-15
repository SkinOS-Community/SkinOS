# Counter-Strike / GoldSrc: Windows SDK preflight

Read-only audit on 2026-10-09. No Wine, game, Steam or UI process was started or stopped by this audit; no game files, prefix or credentials were modified. The sources below are the installed game and the exact NotProton revision embedded in SkinOS, not assumptions about current upstream.

## Failure identified

The supplied AppID 10 report reaches genuine runtime authentication, subscription verification and `launch_ready`, then reports `missing_sdk_path`. At audit time `crates/compat/src/lib.rs` searched only `steam_api64.dll` and always selected the 64-bit `sdk-probe.exe`. The installed game actually has **32-bit `steam_api.dll`**. This is a preflight selection failure; the log does not demonstrate a failed game launch or a failed account login.

Installed evidence:

- Manifest: `$HOME/Library/Application Support/Steam/steamapps/appmanifest_10.acf`; name Counter-Strike, build `12934623`, install directory `Half-Life`, state flags `4`.
- Game root: `$HOME/Library/Application Support/Steam/steamapps/common/Half-Life`.
- SkinOS's cached PICS entry selects `hl.exe` with `-steam -game cstrike` for Windows. The cache also contains macOS/Linux `hl.sh` entries, but the local installed platform is Windows.
- `hl.exe`, `hw.dll`, `cstrike/cl_dlls/client.dll`, `steam_api.dll` all have PE machine `0x014c` (i386). The last file is 263080 bytes and SHA-256 `1ac139ebad2a653adff5700347274cf9816256eb5d69ae6dc43c4cf9c8532aa7`.

## Actual installed SDK ABI

The following exports were read directly from that DLL's PE export table:

| Export | Available |
|---|---|
| `SteamAPI_Init` | Yes; legacy boolean success |
| `SteamAPI_InitSafe` | Yes |
| `SteamAPI_InitFlat`, `SteamAPI_InitEx` | No |
| `SteamAPI_SteamUser_v021` | Yes |
| `SteamAPI_ISteamUser_BLoggedOn` | Yes |
| `SteamAPI_ISteamUser_GetSteamID` | Yes |
| `SteamAPI_Shutdown`, `SteamAPI_GetHSteamUser` | Yes |
| `SteamInternal_FindOrCreateUserInterface` | Yes |

Use a matching i386 probe and the DLL's own flat functions; do not reinterpret x86 C++ return conventions. Disassembly with local LLVM confirms:

- `SteamAPI_Init` at RVA `0x9680` returns a boolean in AL.
- `SteamAPI_ISteamUser_BLoggedOn` at RVA `0x1680` accepts the user pointer on the stack, moves it to ECX, and delegates to vtable slot 1. The flat caller uses cdecl.
- `SteamAPI_ISteamUser_GetSteamID` at RVA `0x54c0` accepts the user pointer on the stack, handles the C++ hidden return buffer internally, and returns the 64-bit ID in EDX:EAX with a plain `ret`. Thus the flat signature is `uint64_t (__cdecl *)(void *)`.

The SDK contains `SteamUser021`, `SteamClient017` and `SteamClient020` interface literals. `hw.dll` also contains `SteamUser023` and imports the generic interface factory. Do not assume the single exported SteamUser accessor version is the only interface the game can request. A preflight must report the actual init/accessor path it used and still check real online state and the expected SteamID.

## NotProton already carries an x86 bridge

The pinned revision is [`bd0fcaa22e0ea28ac67d861fc1c4e7611207f696`](https://github.com/NotProtonNot/NotProton/tree/bd0fcaa22e0ea28ac67d861fc1c4e7611207f696). Its [bridge payload](https://github.com/NotProtonNot/NotProton/blob/bd0fcaa22e0ea28ac67d861fc1c4e7611207f696/app/Sources/NotProtonApp/Model/BridgePayload.swift) includes `i386-windows/lsteamclient.dll`, x86-64 and native ARM64 components. The embedded gzip artifacts were inspected: both `steamclient.dll` and `i386-windows/lsteamclient.dll` are PE machine `0x014c`.

The [runner](https://github.com/NotProtonNot/NotProton/blob/bd0fcaa22e0ea28ac67d861fc1c4e7611207f696/dylib/feats/compat_run.sh#L942) installs the i386 trigger into Wine's `syswow64`. The [shim build source](https://github.com/NotProtonNot/NotProton/blob/bd0fcaa22e0ea28ac67d861fc1c4e7611207f696/steam-shim/build.sh#L7) explicitly says its one x86-64 `steam.exe` serves 32-bit games via WOW64. Therefore a second x86 Steam shim is not indicated by this failure; the probe itself must match the loaded game's SDK architecture.

The [native bridge](https://github.com/NotProtonNot/NotProton/blob/bd0fcaa22e0ea28ac67d861fc1c4e7611207f696/lsteamclient/unixlib.cpp#L796) resolves `steamclient.dylib` using `STEAM_COMPAT_CLIENT_INSTALL_PATH`; SkinOS must continue routing this to its own verified runtime and IPC namespace.

## Verification after the SDK fix

The genuine 32-bit Counter-Strike SDK confirmed the expected online SteamID through `SteamAPI_Init`. The initial approximately three-second check reached `running → stopping → exited` and clean runtime release. It verified the SDK and process startup, **not a working menu**: the short stop concealed a later path-resolution crash subsequently reported by the user. Its [startup report](../../outputs/counterstrike-legacy-launch-check.json) and [SDK proof](../../outputs/counterstrike-sdk-proof.json) remain historical evidence; they are superseded for game stability by the longer check below. The separate [Soundpad x64 regression](../../outputs/soundpad-sdk-width-regression.json) passed.

The later Counter-Strike crash was traced to `hl.exe` dereferencing the missing result of `strrchr` after a failed `GetLongPathNameA`. A controlled Win32 probe established that Wine needed directory enumeration along the existing allowed paths. The production sandbox now permits listing only literal ancestor directories of those paths; it does not grant recursive access to other Steam files. The genuine sandbox regression test verifies both directory listing and denial of an unrelated `Steam/private.fixture` file. See [crash diagnosis and A/B evidence](goldsrc-crash.md).

The current packaged core held Counter-Strike running for **62.94 seconds**; the user confirmed the normal menu. The run recorded no fatal Wine diagnostic and no original Steam GUI, then completed `running → stopping → exited` with runtime exit code 0 and clean release. Evidence: [stability check](../../outputs/counterstrike-stability-check.json), [runtime proof](../../outputs/counterstrike-stable-runtime-proof.json). This corrected build is installed at `dist/SkinOS.app`.

The selected Windows installation has scoped write permission for local GoldSrc settings; 14 pre-existing CFG files were backed up under SkinOS before that change. The engine's SDL/OpenGL and CRT dependency chain was sufficient to reach the observed menu, but this does not establish every rendering mode, a network match, or long-session stability. Missing SDL3/video and route warnings from the host were not the cause of either the original SDK selection failure or the identified executable-path crash.
