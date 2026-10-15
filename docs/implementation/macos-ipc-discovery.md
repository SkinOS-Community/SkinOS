# macOS SDK discovery: static findings, 2026-10-09

This note records static inspection only. No SDK, host, broker, game or official Steam process was started for this investigation. Live tests were explicitly stopped after the user reported official Steam opening.

## Observed failure

The independent SDK probe, executed earlier by the runtime worker, reported:

- `client_library_matches: true`
- `init_result: 1`
- `sdk_initialized: false`
- `Could not determine Steam client install directory.`

The original SDK stderr additionally reported `ipcserver init failed`. This establishes that selecting the correct `steamclient.dylib` image is insufficient: the normal macOS SDK also needs its discovery service.

## Original Brotato SDK, arm64

Static exports: `SteamAPI_InitFlat` at image offset `0x3b54`, `SteamAPI_RestartAppIfNecessary` at `0x4158`, `SteamAPI_IsSteamRunning` at `0x4a74`.

`SteamAPI_InitFlat` forwards to the real internal SDK initialization function. Discovery helper `0x35d8` initializes a Mach connection to `com.valvesoftware.steam.ipctool` through `bootstrap_look_up` (call at `0x5700`), checks protocol version 104 (`0x68`), then `0x3690` requests the registered Steam path and PID. It sends opcode 14 and tests whether that PID is alive before accepting the path.

The existing blanket sandbox denial for `com.valvesoftware.*` can therefore explain the observed `ipcserver init failed` without any binary ABI error. Removing this denial alone would allow discovery of the existing registered service; it would not establish that the service belongs to SkinOs.

### A concrete route that can open the official GUI

The SDK's sole imported `_system` call is at `0x450c`, inside `SteamAPI_RestartAppIfNecessary`. It formats `/usr/bin/open '%s'` with a `steam://run/<appid>` URL. That is a confirmed mechanism, not proof that this branch caused the user's particular observation.

The same function checks `SteamAppId` at `0x41a4` and converts it with `atoi`. A present nonzero value returns false at `0x41fc–0x4200`, before the URL-opening path. Thus the actual launched game's environment must be checked, including across app-bundle or LaunchServices spawning boundaries; setting the variable only in the parent is not proof that the game receives it.

The inspected InitFlat entry path does not directly call `_system`. Its Mach lookup may activate a registered service through launchd. The inspected `ipcserver` binary itself has no `system`, `exec`, `posix_spawn`, LaunchServices-open or URL-opening imports, so a direct GUI-launch claim about that broker is not established by these static findings.

Steamworks documents that `SteamAPI_RestartAppIfNecessary` can start the Steam client and relaunch through a `steam://run/` URL. [Valve documentation](https://partner.steamgames.com/doc/api/steam_api#SteamAPI_RestartAppIfNecessary).

## Original macOS ipcserver, arm64

The binary is a universal executable, not a shared library. No command-line parser imports or argument-driven startup were found. Its main entry at `0x100001ae8` calls:

`bootstrap_check_in(bootstrap_port, "com.valvesoftware.steam.ipctool", &receive_port)`

at `0x100001b8c`. If check-in fails, it returns zero without starting the service loop. Directly executing this binary therefore does not itself register the Mach service. A launchd/bootstrap registration or explicit process-local bootstrap adapter is a separate requirement.

The dispatcher at `0x1000017a0` reads an opcode at request offset `0x18`. Its jump table is at file offset `0x33e4` in the thin arm64 slice:

| Operation | Target | Observed behavior |
|---|---|---|
| 13 | `0x100001910` | Register a live owner PID from request `+0x1c` and path from `+0x20`; existing live registration is retained |
| 14 | `0x1000019b8` | Check registered PID and path readability; reply with PID at `+0x1c`, path at `+0x20`, response size `0x220` |
| 100001 | `0x100001824` | Reply with protocol version 104 |
| 100000 | `0x10000183c` | Shutdown/cleanup branch gated on owner PID |

Other operations maintain real shared-memory/semaphore bookkeeping. Implementing only a fake GetSteamPath success would not establish a working SDK transport and is not proposed.

## What this implies for the next implementation

1. `SetClientUIProcess` and the normal client-UI mode setup must be investigated against the current engine. OpenSteamClient calls these methods; they are likely involved in publishing the process identity, but the exact current Mac method-to-opcode call path is not yet verified.
2. The host and SDK must share a deliberately owned discovery broker. Pinning the client DLL does not select the Mach service owner.
3. A possible architecture is an explicitly launched copy of Valve's broker with a private service registration and a narrowly scoped bootstrap-name adapter in SkinOs processes. This is a design candidate, not a validated recipe, and requires correct lifecycle/cleanup and collision handling. No launchd registration was changed in this investigation.
4. Before resuming any live launch, prevent out-of-process application activation as well as direct process execution. Record actual game `SteamAppId` propagation; do not infer it from the parent host environment.
5. Only after those controls are implemented should the original SDK probe be rerun with the independently verified host PID and official GUI processes absent.

Static tools: `nm`, `strings`, `otool`, `llvm-objdump`, and a thin arm64 copy made with `lipo`. Addresses above refer to these inspected builds; do not reuse them as ABI for another build.

## Private bootstrap namespace assessment

Apple's historical `bootstrap_subset` contract does support child-only names and shadowing a service from the parent namespace; destruction of the requestor port tears down the subset. However, the currently installed SDK marks `bootstrap_subset` and `bootstrap_create_server` deprecated from macOS 10.10. Breakpad's maintainers explicitly reported both APIs broken on 10.10; consequently that architecture requires a benign isolated capability test on the actual OS before it is chosen. No such mutation/test was performed under the current read-only restriction. [Apple bootstrap header](https://github.com/apple-oss-distributions/launchd/blob/main/liblaunch/bootstrap.h), [first-party Breakpad failure report](https://bugzilla.mozilla.org/show_bug.cgi?id=1052807#c10).

A currently running original `ipcserver` was observed by the parent agent. The safest routing design must therefore distinguish the owned broker, rather than merely permitting `.ipctool` lookups. If the deprecated subset route is unavailable, a narrowly scoped bootstrap transport adapter can route the standard fixed name to an owned Mach endpoint while still executing the genuine Valve broker and its actual protocol. An inherited Mach send-right/rendezvous design would avoid modifying launchd job definitions; that is a candidate for further API verification, not an implemented component.

## Implemented private broker and validation

The approved implementation now lives in `crates/runtime-host/src/broker.rs` and `native/ipc.c`:

- A nonce-labelled `dev.skinos.ipc.<nonce>` job is bootstrapped only into the current user's GUI domain from a mode-0600 plist inside the private runtime state. Its program is the copied genuine Valve `ipcserver`; no persistent LaunchAgents file is installed.
- Its sole Mach service is `dev.skinos.ipctool.<nonce>`. No original Valve job is bootstrapped, replaced, unloaded, or reused.
- The combined adapter maps exact `com.valvesoftware.steam.ipctool` lookups/check-ins to that configured name. Missing/invalid namespace and all other `com.valvesoftware.*`/`com.valve.*` requests fail closed; unrelated services remain unchanged.
- `skinos_ipc_is_configured()` is a pure preflight with no service lookup. `Broker` owns cleanup; startup failures and Drop boot out only its generated exact label.

Validation completed without any Steam engine, SDK, account login, or game execution:

1. C negative-routing tests passed with fake underlying functions: missing/invalid namespace and `.other` names never reached an original lookup/check-in.
2. Rust plist/path tests passed; the runtime-host crate compiled with the new native adapter.
3. The genuine copied broker started under its private job. A small C protocol client ran inside a sandbox explicitly denying original Valve Mach-service lookups. The fixed Steam service name and the configured private name resolved to the same Mach port, and an actual protocol-version request returned 104.
4. Drop removed the generated job; an exact-label `launchctl print` failed afterward, confirming cleanup. A failed preliminary test also cleaned up its own job.

Proof: `outputs/broker-route-validation-1791487656296071000/validation.json`. This proves discovery routing and genuine broker transport, not yet successful SteamAPI initialization or game identity; those require the runtime agent's next guarded integration test.

## Supervisor-crash cleanup

A later integration test revealed that SIGKILL of the Rust supervisor bypasses `Broker::Drop`. The copied broker now also contains an owner-lifetime watcher in the injected adapter. This runs only for `SKINOS_COMPONENT_ROLE=broker`, requires matching generated `dev.skinos.ipc.<nonce>` / `dev.skinos.ipctool.<nonce>` values, and reads only its private `runtime-owner.pid` file without following symlinks.

`Broker::start` initially writes the supervisor PID. Main must replace that file atomically with the actual staged worker PID immediately after spawn; the worker can also write its own PID at entry to close the supervisor-crash handoff window. Missing/dead owners receive a bounded startup/handoff grace. Once the owner is dead, the watcher calls `launchctl bootout gui/<uid>/<exact-own-label>` with no inherited adapter environment. It never signals or kills the runtime worker, game, or original Valve processes.

Validation `outputs/broker-watchdog-validation-1791488580967132000/validation.json` killed only a scratch supervisor with SIGKILL: the broker remained available while a dummy `sleep` worker was alive, then its exact launchd job disappeared after that worker exited. No Steam engine, SDK, real account session, or game was started by this cleanup test.

## GetSteamPath regression telemetry

A later run reported `ipcserver GetSteamPath failed` after moving the worker beside the staged client library. `ipc.c` now temporarily traces only discovery opcodes 13 and 14 inside the genuine broker role, reporting PID, path, liveness and `access(R_OK)` result. Other Mach payloads, engine traffic and credentials are not inspected.

A controlled test registered a real dummy client PID and its actual executable path through opcode 13, then queried opcode 14. Both the repository output directory and `~/Library/Application Support/SkinOs/runtime/.../runtime/dummy-client` passed: the genuine broker returned the exact PID/path, and its log showed `alive=1 path_readable=1 path_errno=0`. Thus the location under AppSupport alone does not explain the real worker's failure. The next runtime investigation must observe whether its actual `SetClientUIProcess` call registers the intended PID/path.

Evidence: `~/Library/Application Support/SkinOs/runtime/broker-discovery-validation-1791490411714339000/validation.json`. This test used no Steam engine, SDK, account login or game. Both own jobs were removed after validation.

## Truthful explicit process publication

The runtime agent's controlled global/bootstrap-only run produced no discovery opcode 13 after calling the inspected `SetClientUIProcess` stub. Therefore calling that stub is not accepted as proof of publication on this build.

`ipc.c` now exports two narrow metadata helpers:

- `skinos_ipc_query_process(int *pid, char *path, size_t capacity)`: queries the real private broker via opcode 14; returns 1 for real metadata, 0 for empty registration, -1 for transport/configuration failure.
- `skinos_ipc_register_current_process(void)`: requires runtime role, derives `getpid()` and canonical `_NSGetExecutablePath`, verifies that executable is in the configured per-run `runtime/`, sends actual opcode 13 to the owned broker, and returns success only after a real opcode-14 reply matches both values.

The SDK expects the **executable file path**, not merely its directory: downstream code finds its last slash and replaces the basename with `steamclient.dylib`. No SteamID, login result, license, Steamworks callback or API success is fabricated by these metadata helpers.

Actual private-broker validation with a dummy executable under AppSupport returned `registered=1 queried=1 pid_matches=1 path_matches=1`; cleanup was verified. Evidence: `~/Library/Application Support/SkinOs/runtime/broker-publication-validation-1791490729495566000/validation.json`.

The legacy `launch_msg` entry point is now also guarded: StopJob/StartJob/RemoveJob requests for original `com.valvesoftware.*` / `com.valve.*` jobs fail with `NULL` and `errno=EPERM` without forwarding. Pure tests use fake underlying counters and confirm zero original-job calls. This closes the SDK's version-mismatch fallback route that otherwise could target the original broker even after Mach-name routing.
