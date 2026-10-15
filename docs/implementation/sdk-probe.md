# Original Steamworks SDK consumer probe

`crates/sdk-probe` builds `target/debug/skinos-sdk-probe`. It is a separate consumer of the game's original `libsteam_api.dylib`; it does not emulate Steamworks, fabricate callbacks/identity, authenticate, or call `SteamAPI_RestartAppIfNecessary`.

## Verified exports and ABI

Static `nm -arch arm64 -gU` inspection of the downloaded Brotato macOS library found:

- `SteamAPI_InitFlat(SteamErrMsg*) -> ESteamAPIInitResult` (integer 0 is success).
- `SteamAPI_SteamUser_v023() -> ISteamUser*`.
- `SteamAPI_ISteamUser_GetSteamID(ISteamUser*) -> uint64_steamid`.
- `SteamAPI_ISteamUser_BLoggedOn(ISteamUser*) -> bool`.
- `SteamAPI_RunCallbacks()` and `SteamAPI_Shutdown()`.

There is **no exported `SteamAPI_Init`** in this library. Modern Valve headers inline that function and recommend `SteamAPI_InitFlat` for dynamically loaded flat bindings. The probe uses that actual SDK entry point and checks the versioned user factory separately. [Valve SDK header](https://github.com/ValveSoftware/Proton/blob/proton_11.0/lsteamclient/steamworks_sdk_164/steam_api.h), [Valve flat-interface header](https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/steam/steam_api_flat.h), [Steamworks initialization documentation](https://partner.steamgames.com/doc/api/steam_api).

Both arm64 and x86_64 slices exist in the downloaded library. The current executable tests its own architecture; it does not prove the other slice works.

## Invocation

```text
skinos-sdk-probe \
  --library <game>/Brotato.app/Contents/MacOS/libsteam_api.dylib \
  --appid 1942280 \
  --expected-steamid <authenticated-account-id> \
  --client-library <runtime-state>/runtime/steamclient.dylib
```

The runtime host must already be authenticated and stay alive. Launch this process inside the host's isolated sandbox with these inherited values:

- `SKINOS_RUNTIME_STATE_ROOT=<runtime-state>`, the explicit disposable per-run state containing `runtime/`.
- `SKINOS_RUNTIME_DATA_ROOT=<account-data>`, an existing private account-data directory; it may persist outside the disposable per-run state.
- `DYLD_INSERT_LIBRARIES=<runtime-state>/libskinos_trace.dylib`, the existing fingerprint-specific storage adapter used by the host.

The probe sets only its own `SteamAppId` and `SteamGameId`, changes its own working directory to the explicitly chosen runtime library directory, and preloads that exact staged library. It never changes `HOME`. Client library paths outside `<SKINOS_RUNTIME_STATE_ROOT>/runtime/` are rejected. Parent process must apply a bounded timeout (suggested 15 seconds); native initialization itself is not preemptible within one process.

The parent must permit this probe executable and original SDK file in the sandbox. If its sandbox forbids subprocess execution, use the normal runtime parent as supervisor or explicitly permit this helper. The probe does not spawn another child in dynamic mode.

It polls real callbacks for up to three seconds after successful initialization, reads the real user interface, calls `SteamAPI_Shutdown`, and exits. SDK diagnostics go to stderr. stdout contains one JSON record, with `sdk_initialized`, `logged_on`, observed `steamid`, `expected_matches`, `client_library_matches`, numeric `init_result`, and any failure message. Exit 0 in dynamic mode requires all four success booleans.

## What the result proves

Positive identity requires successful original SDK initialization, non-null SteamUser023, the expected actual SteamID, an online user, and no differently located `steamclient.dylib` image inside the probe. It is not enough that `SteamAPI_InitFlat` returns success.

Library pinning selects the binary image; it does not independently identify the server process behind Steam's IPC. The parent must additionally record the live SkinOs host PID and demonstrate that official Steam processes are absent while the probe runs. SDK inspection found the standard `com.valvesoftware.steam.ipctool` discovery string; an environment variable alone is not treated as proof of IPC peer selection. Full online game-server authentication and gameplay remain separate checks.

## Static mode and checks

Append `--inspect-only` to inspect required exports with system `nm`. This mode never loads Valve code and always reports `sdk_initialized:false`; exit 0 means only that static checks passed.

Initial validation: static export inspection passed for the installed Brotato library; two argument/success-criteria tests and strict Clippy passed. No live-host SDK initialization was performed while preparing this tool.
