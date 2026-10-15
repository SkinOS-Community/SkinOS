# Runtime host: dynamic progress

Updated 2026-10-09 (Asia/Shanghai). Implementation: `crates/runtime-host`.

## Confirmed on this Mac

1. Separate Rust child is confined with a Seatbelt profile; fake canary read/write outside state are rejected, outbound TCP is rejected, own-state writes succeed. HOME is unchanged.
2. Locally installed original Valve runtime is staged as private copies. `steamclient.dylib` SHA-256 is pinned to `035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`. Required transitive dependencies include crashhandler and the full signed Breakpad framework.
3. `dlopen(RTLD_NOW|RTLD_LOCAL)` and `dlclose` succeed.
4. `CreateInterface("CLIENTENGINE_INTERFACE_VERSION005")` returns non-null with code 0.
5. `Steam_CreateGlobalUser` succeeds with user=1 and pipe=1, without `steam_osx` or `steamwebhelper`.
6. Real callbacks are polled and freed. Connection/logon are false because no login is attempted and outbound networking is disabled.
7. User and pipe release, reviewed `BShutdownIfAllPipesClosed`, and `dlclose` all succeed.

Report: [`outputs/runtime-host-global.json`](../../outputs/runtime-host-global.json). Previous individual runs remain under `outputs/runtime-state/probe-*`.

## ABI evidence

The pinned [OpenSteamworks IClientEngine header](https://github.com/OpenSteamClient/OpenSteamworks/blob/d0abbe85f4b8314607836e1c588bb48813f642a4/cpp/include/steamclient/IClientEngine.h) is compared with current arm64 exported wrappers. `Steam_CreateGlobalUser` forwards its `HSteamPipe*` argument to engine slot2; `Steam_ReleaseUser` forwards pipe/user to slot6. `Steam_BGetCallback` has two arguments.

Engine slot8 (`GetIClientUser`), slot14 (`GetIClientUtils`), and slot49 (`BShutdownIfAllPipesClosed`) are confirmed against generated source plus runtime method offsets/disassembly. The latter at `0xa024dc` checks the pipe count and stops the global runtime thread.

[OpenSteamworks UserHelper](https://github.com/OpenSteamClient/OpenSteamworks/blob/d0abbe85f4b8314607836e1c588bb48813f642a4/OpenSteamworks/Helpers/UserHelper.cs) uses `SetLoginToken(refreshToken, username)` then `LogOn(SteamID)` for QR tokens. Current Mac user slot56 at `0x896610` serializes exactly two strings and references the diagnostic name `SetLoginToken`; slot1 at `0x890f10` serializes a SteamID and references `LogOn`. A token from the user’s explicitly created SkinOs Keychain session was later supplied through stdin; genuine native login and SteamID matching passed (root evidence: outputs/mvp-runtime-auth.json).

## Runtime state and remaining work

`GetInstallPath` points to the staged private runtime. However both user base paths initially return the original `~/Library/Application Support/Steam`; sandbox rejects reading/writing that path. Current binary code at `0xe0a70` derives this from `getenv("HOME")` with a `getpwuid` fallback and literal `Library/Application Support/Steam`. No supported override was found in that function. Dedicated storage adaptation is required before authenticated tests.

An IPC bind to `127.0.0.1:57343` is necessary for global creation; denying it causes a native crash after bind/listen assertions. A diagnostic dylib inside **our helper only** records bind addresses and passes calls unchanged. The sandbox now permits this specific listener while keeping outbound networking denied. Existing Steam is never killed; live ownership of the port must be checked before starting a real host.

Controller shared memory and missing optional SDL/video libraries currently produce assertions in the reduced payload. They did not prevent unauthenticated lifecycle, but the probe is not yet a full game host. Auth, ownership, launch configuration, game-side SteamID verification, and runtime lifetime until game exit remain to implement and validate.

## Storage, authentication and stricter platform bootstrap

The storage adapter now redirects exactly the reviewed `snprintf` format/component used by clientdirectories.h into `SKINOS_RUNTIME_DATA_ROOT`. Both base-directory getters match the private root and real config/log files are created there; HOME, original Valve files and executable pages remain unchanged. Each state directory is mode0700. Native login using the explicit SkinOs refresh token succeeds and the actual `IClientUser::GetSteamID` matches the account.

Default runtime source now comes exclusively from `~/Library/Application Support/SkinOs/runtime-source/current.json`; no silent fallback to an installed Steam app. The root task staged binary-only copies with verified hashes.

A real Brotato LaunchApp request reached native ownership=true, installed state4 and launchoption2. A first crash was traced to `BIsGameRunning`: the current Mac wrapper dereferences its CGameID argument (`0x8976ac`), despite the C# declaration omitting `in`. It is now passed by reference. Subsequent LaunchAppResult=0 alone was **not** accepted as proof of a playable game, since it can represent a transient bootstrap process. The genuine SDK check remained failed at install-directory/ipcserver discovery.

The user reported the original Steam GUI opening. All live attempts were stopped immediately; process checks found no surviving SkinOs host/SDK/game. The original ipcserver was not killed or reused. Static disassembly confirmed native Steam’s Mac launch path calls NSWorkspace (`0xd6448…0xd64f8`), so a mere process-exec allowlist cannot constrain LaunchServices’ separate process launch. Separately, Brotato’s unmodified SDK RestartAppIfNecessary can open steam://run when SteamAppId is absent. No source conclusively attributes the observed GUI to one particular user/agent action; the protection must cover both paths.

The new platform adapter replaces only the reviewed NSWorkspace launch methods inside our runtime process. It rejects URL schemes, any nonselected application, and missing/mismatched SteamAppId/SteamGameId. For the selected bundle it will start the real CFBundleExecutable as a child under the sandbox with the genuine Steam-generated context, retaining a dedicated owned IPC namespace. It returns the actual OS PID to Steam’s existing process monitor. It never fabricates SteamSDK, license, or authentication results. The IPC/directory adapter is now deliberately intended in that selected game as well, to route its unmodified Steam SDK to our own broker; this is a documented change from the initial helper-only scope.

Negative tests already run without loading Valve code or launching any application: eight context cases, verified NSWorkspace replacement pointer, rejection of steam://, HTTP, Steam.app and missing-context game targets. Missing IPC adapter/namespace now blocks even runtime startup. Broker implementation and final real-game+SDK proof remain in progress.

## End-to-end result reached

The real SDK preflight now passes: `SteamAPI_InitFlat = 0`, `sdk_initialized`, `logged_on`, `expected_matches`, and `client_library_matches` are all true. Evidence: [`outputs/runtime-sdk-proof.json`](../../outputs/runtime-sdk-proof.json). The game’s unmodified Steam API library connects through our unique genuine Valve broker to our authenticated genuine Valve engine. No Steam authentication/entitlement responses are simulated.

The missing bootstrap step was explicit genuine broker operation13 publishing **this worker’s real PID and executable file**, followed by genuine operation14 to verify both. `SetClientUIProcess` alone did not publish it. The worker executable is staged next to `steamclient.dylib`, so SDK discovery derives the correct install directory. The SDK preflight temporarily counts as a running app; the host drains frames/callbacks until its exited process is removed before asking real `LaunchApp` to start the game.

The controlled run then reached `running` only after positive SDK proof, LaunchAppResult0, BIsGameRunning and kernel `proc_pidpath` matching the selected real Brotato executable. The user had separately confirmed an earlier visible launch without original Steam; the earlier failed companion probe was not treated as a successful SDK proof. Final visual/menu verification is coordinated by the root task.

## Persistent saves and lifecycle

Steam runtime data and library manifests now live persistently under SkinOs `accounts/<SteamID>/runtime`, mode0700. Per-account flock prevents concurrent runtime writers and survives worker exec; a regression verifies saved data survives reopen and a second runtime is rejected. Diagnostics and executable copies remain per run.

Brotato’s separate native Godot save root is its established `~/Library/Application Support/Brotato`, verified from shipped project config and Steam UFS metadata. It receives an explicit single-game sandbox grant; HOME and original Steam account directories remain restricted. The root task backed up and hash-verified16 existing files before enabling normal game writes. A create/write/fsync/rename/remove probe inside that exact root passed without modifying existing files; the successful run’s game stderr has no save or Steam initialization errors. Other titles need their own verified native save policy before launch.

Game stdin is `/dev/null`; stdout/stderr use private per-run logs, avoiding inherited control pipes and allowing host cleanup independently of game log handles. The owned broker job has exact-label cleanup plus a watchdog tracking the actual worker PID; the agent validated supervisorSIGKILL with a dummy worker, preserving the broker until that worker exited and then removing only its own job. Original Valve jobs are never stopped.
