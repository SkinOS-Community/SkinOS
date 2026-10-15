# Genuine Steam Input host: pinned macOS runtime

Status as of 2026-10-14: the owned Valve host discovers the software controller,
loads the original stock configuration and exposes genuine Steam Input handles
to native and Windows SDK consumers. The separate Wine transport passes its
XInput and DirectInput conformance checks. **The complete genuine Steam Input
→ Wine input chain passed a labelled software-fixture press/release test.**
Ordinary game launch and the product diagnostic now share automatic Wine input
preparation. Both passed with zero devices and a labelled software fixture.
Physical hardware verification is explicitly deferred by the user.

Ordinary per-game Steam Input now also selects the retained feedback channel.
Real Windows XInput requests reached the original SDL fixture through the
genuine SDK, including three-second retained effects and final zero. An ordinary
session delivered 81 requests without the former 64-entry trace limit. Input and
SDK feedback both recovered across a real driver generation change during the
same running game. See [feedback evidence and boundaries](steam-input-rumble.md).

These are distinct results. `ISteamInput::Init`, a nonzero controller handle,
an accepted output frame, and actual button/axis delivery are not interchangeable
proofs. Software fixtures never count as physical-controller verification.

## Owned runtime and initialization

Engine SHA256: `035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`.

The initial missing-`libSDL3.dylib` failure was resolved by staging the pinned
`libSDL3.dylib`, `libsteaminput.dylib` and `controller_base/` cohort. Staging checks
the engine identity, manifest, library hashes, regular files and path allowlist;
account files and controller user configurations are excluded. There is no
runtime lookup of the original Steam installation. See
[`stage-runtime-input.py`](../../scripts/stage-runtime-input.py) and
[staging evidence](../../outputs/steam-input-source-stage.json).

The engine constructs its controller host and calls
`Init(true, nullptr, 0, false)` itself. Its main loop already pumps controller
frames. No duplicate initialization or additional private frame pump is needed.
The following addresses are audited for the exact ARM64 fingerprint, not a
portable Valve ABI:

| Object/method | Slot | Address |
|---|---:|---:|
| IClientEngine GetIClientController(pipe) | 53 | `0xa05608` |
| IClientEngine GetIClientControllerSerialized(pipe) | 66 | `0xa05514` |
| CSteamController Init | 2 | `0x6b5cf8` |
| CSteamController Shutdown | 3 | `0x6b6eec` |
| CSteamController RunFrame | 4 | `0x6b71a4` |

RTTI, matching proxy/dispatch tables and bounded disassembly establish these
seams. The earlier large-engine Hopper timeout is retained as a failed analysis,
not described as successful decompilation. See
[engine vtable](../../outputs/steam-input-research/engine-vtable-static.json),
[controller vtables](../../outputs/steam-input-research/controller-vtables-static.json)
and [call sites](../../outputs/steam-input-research/controller-calls-static.json).

## Genuine game context, focus and stock configuration

The Windows probe initializes the game's unmodified Steamworks DLL, borrows its
actual user/pipe, verifies the account and checks `SteamUtils::GetAppID` before
using `SteamInput006`. Raft's real SDK context is AppID 648800. The native probe
provides an independent consumer under the same owned host.

Per-app focus is separate from SDK identity. The audited Utils setter/getters
verified the roundtrip `769 → 648800 → 769`, with 648800 retained through the
scans. The initial-state guard refuses to overwrite another active game/process.
Adding `-forcecontrollerappid` to the worker was not used: the matching parser
was found in Steam UI, not the pinned engine.
[Focus evidence](../../outputs/steam-input-focus-state-proof.json),
[Valve per-app mapping documentation](https://partner.steamgames.com/doc/features/steam_controller/getting_started_for_devs?language=english#Step3).

The stock configuration ABI is now recovered and implemented in
[`input_config.rs`](../../crates/runtime-host/src/input_config.rs):

| Call | Serialized slot / proxy | Verified arguments after `this` |
|---|---|---|
| WarmOptInStatus | 40 / `0x944994` | controller index, AppID |
| LoadConfigFromVDFString | 36 / `0x944284` | index, AppID, VDF, optional app-info pointer, configID, settings pointer |
| ActivateConfig | 39 / `0x9447fc` | index, AppID, `false` |

The exact original UI fallback supplies a required 28-byte settings structure:
`0100000000000000ffffffff40010000ffffffff0000000001000000`.
Fields whose semantics remain unknown stay explicitly opaque; their observed
values are used without inventing general per-game settings. The original
fallback app-info pointer is null. The pinned
`controller_generic_gamepad_joystick.vdf` has SHA256
`57d41acf506a8a492852d465a4531d9068288538e4736dea92a5f80ed9e523fa`
and stock template configID 4. The host verifies the actual fixture index before
dispatch; it does not assume controller 0 from a zero-initialized value.
[Exact caller/metadata audit](../../outputs/steam-input-research/config-stock-caller-audit.json).

These void calls report only `calls_dispatched`. Subsequent real native and
Windows scans independently confirmed a nonzero controller handle, controller
index 0, type 4 and gamepad slot 0; the native scan also saw fresh input frames.
The earlier zero-handle/focus-only reports are historical failures, superseded
by this stock-layout result. They are no longer the current controller-discovery
status. [Stock configuration](../../outputs/steam-input-stock-config-check.json),
[native/Windows differential](../../outputs/steam-input-stock-config-native-then-windows.json).

`SetSelectedConfigForApp` is a separate persistent-selection operation. Direct
Load/Activate works without it for this diagnostic; arbitrary saved user layouts
are not claimed as supported. `GetDeviceBindingRevision` describes action-binding
revisions and is not, by itself, proof of a legacy XInput route.
[Valve API reference](https://partner.steamgames.com/doc/api/ISteamInput#GetDeviceBindingRevision).

## macOS output publication failed; Wine output is separate

REA/Hopper established the original `libsteaminput` Create/Send/Destroy path:
Create calls `IOHIDUserDeviceCreateWithProperties`, Send calls
`IOHIDUserDeviceHandleReportWithTimeStamp`, and Destroy cancels the device.
The owned host's live trace subsequently recorded **three null Create results**.
The restricted HID entitlement was absent; no macOS virtual gamepad was
successfully published by that route. This is no longer merely an untested
permission concern. [OS publication evidence](../../outputs/steam-input-os-hid-check.json),
[REA Create evidence](../../outputs/steam-input-research/steaminput-create-evidence.json).

The Wine transport uses an owned PE bus driver inside the owned Wine prefix,
with stock `winexinput.sys`, `winehid.sys` and `hidclass.sys`. It does not modify
installed CrossOver binaries or require macOS HID publication. The producer
boundary preserves the verified original 188-byte descriptor and 20-byte mapped
report. A presentation adapter converts those real semantic values into Wine's
HID usages. The basic driver remains input-only. The separately verified feedback
driver adds two-motor Haptics feature/output reports without changing the native
producer descriptor or input representation; normal game feedback selects it.
[Implementation and protocol](../../third_party/notproton/integration/wine-input/README.md).

The following **transport-only** live checks passed under the selected CrossOver
Preview FEX environment:

| Suite | Result | What it proves |
|---|---|---|
| XInput FEX11 | 54 cases passed; cleanup confirmed | Create-only neutral, buttons, D-pad combinations, triggers, signed axis extremes, combined state, release, detach |
| DirectInput DI3 | 59 cases passed; cleanup confirmed | Real DirectInput states and XInput comparisons, neutral-ready reconnect, held-state EOF removal and clean reopen |
| DirectInput x86 consumer / ARM64 device service | 59 cases passed; cleanup confirmed | The same state/lifecycle suite through a real 32-bit consumer in the FEX prefix |

[XInput summary](../../outputs/wine-input-conformance-fex-11/logs/summary.json),
[DirectInput summary](../../outputs/wine-input-conformance-di-3/logs/summary.json),
[32-bit consumer summary](../../outputs/wine-input-conformance-di-x86-1/logs/summary.json).
The x64 PE consumer is built but has not been run in this conformance series.
Temporary test prefixes may be removed after cleanup; reports and payload
manifests remain retained. These tests feed explicit labelled fixture reports,
so they prove the Wine transport rather than the complete Valve producer chain.

Device ownership is checked through real `DIPROP_GUIDANDPATH`, including the
complete IG interface path, slot and generated serial. Product names are not
used as ownership evidence because Wine renames them. The tests reject another
gamepad rather than accepting whichever global XInput slot happens to respond.

DirectInput's legacy IG device combines left/right triggers and omits Guide;
XInput tests the independent triggers and Guide. DI's API-reset centre can be
32768 while the processed inverted-Y centre is 32767; that one-unit distinction
is accepted only for original Y/RY 0 and reported explicitly. Other values remain
strict. Actual target Y extremes passed; an earlier source-only prediction of a
signed16-Y failure did not reproduce in this CrossOver build.

## Verified software-fixture end-to-end check

`steam-input-wine-neutral-4` and `steam-input-wine-activity-2` passed. The latter
uses the real owned Valve host, original stock layout, Raft's unmodified SDK and
an independent Windows SteamInput/XInput consumer. It verifies the app/account,
reciprocal controller-handle/slot mapping, two stable neutral samples, SDL A +
left-X=32767 readback, two mapped pressed samples and release. The genuine mapper
returns Y=1 for SDL center=0 after the first update; the checker explicitly
allows one signed16 quantization unit only at centered axes. Buttons, triggers
and full deflection remain exact. Two actual XInput state changes were observed.
No physical controller was used. No original Steam GUI was observed in 87 process
samples; runtime, relay, shim and temporary bus cleanup all completed.
[Run](../../outputs/steam-input-wine-activity-2/summary.json),
[Probe and fixture proof](../../outputs/steam-input-wine-activity-2/steam-input-proof.json),
[Artifact hashes and final transport/shutdown proof](../../outputs/steam-input-wine-milestone.json).

Two lifecycle repairs were necessary: reuse the SDK-prepared prefix without
upstream's repeated `WINESERVER -k`, and track the actual input-probe process
separately from the Steam shim. The original shim waits for Wine session shutdown,
which the live feeder intentionally postpones. It is retained until Valve and
relay teardown; only then is its exit validated and the diagnostic published.
Kernel process termination is separate from a JSON summary; unknown actual
probe exit metadata remains null.

## Opt-in game integration and product diagnostic

The per-game `steam_input` preference defaults off. The ordinary `launch_game`
command inventories the original loaded SDL, correlates its device with a fresh
Valve log entry, applies the exact source-device filter, validates the genuine
SDK context, ensures the owned Wine driver, and loads the original stock layout.
It does not assume controller index zero. One SDL gamepad is supported; ambiguous
VID/PID filtering, additional controllers and unsupported raw-HID routes are
rejected with an explanation.

Soundpad's normal launch/stop passed both without a controller and with a neutral
software fixture. This verifies integration and cleanup, not gamepad gameplay in
Soundpad. Background Soundpad services can hold Wine's launcher open after the
actual game exits; cleanup then reaps only the owned launcher and records the
real forced-launcher result separately. Other services remain untouched.
[Normal launch evidence](../../outputs/game-input-normal-milestone.json).

`diagnose_input` temporarily selects the same preparation path, independently of
the saved toggle. A zero-device scan is a valid diagnostic, never input success.
The explicit software fixture remains labelled; there is no synthetic controller
in a default product diagnostic. Root/service preparation is manifest-checked and
prefix-scoped. A removed historical test driver left an orphan SYS in Raft's
prefix; the new version correctly refused it until its exact prior-test SHA and
root/service absence were verified and only that file removed.
[Cleanup evidence](../../outputs/steam-input-old-test-driver-cleanup.json).

The diagnostic distinguishes raw sample changes from meaningful activity. One
unit of mapper centre quantization is retained in raw data but does not label a
neutral controller as receiving presses/movement. The exact versioned thresholds
are diagnostic-only and never change game input.
[Activity policy and regression evidence](steam-input-activity.md).

A disconnected source now waits for genuine Valve Destroy and its Wine driver
ACK, then for a fresh SDL identity and fresh Valve index with the same
VID/PID/type/virtual-source classification. The original prepared filter must
remain valid and unambiguous. The stock configuration is reapplied; reconnect
is announced only after a new genuine Create ACK, both active masks equal one,
and the actual controller index maps to XInput slot zero. The game is not
restarted. Absence waiting is nonblocking and has no overall time limit;
Destroy and new binding/output retain bounded two/five-second deadlines.

The normal Soundpad run passed `ready → disconnected → waiting_reconnect →
reconnecting → reconnected → stopped`. A separate Windows consumer observed
one real owned IG interface (generation 1), complete DirectInput/XInput removal,
and a new owned path (generation 2) with matching actual near-neutral states.
The observer never writes input. Its one-unit tolerance handles the original
mapper's centre quantization; the strict transport tests remain unchanged.
[Live readbacks](../../outputs/game-input-reconnect-2.json),
[runtime lifecycle](../../outputs/steam-input-reconnect-runtime-proof.json).

The same run exposed a real lifecycle detail: the tracked game's PID was
confirmed gone by the kernel while the SDK still reported the AppID active.
Game-mode teardown now uses that bound PID termination, retains the late-fatal
check and records unavailable numeric exit as null. It does not wait forever
for unrelated service/shim activity, and does not kill those services.

A controller first connected after a zero-device launch, a different model or
ambiguous/multiple sources still needs a new launch. Failures disable only
input and explain the result while the running game continues.

Late DirectInput acquisition now uses a joined 16 ms driver worker to satisfy
pending HID reads from the latest real mapped report. It does not add producer
frames or overwrite newer cached releases. A live isolated prefix passed 21
comparisons: two new DirectInput consumers acquired after exactly one held
producer report, release remained released, EOF removed both interfaces, and a
new device started neutral without a new report. Root/service removal and worker
shutdown completed. This is transport-only evidence.
[Late-acquire proof](../../outputs/wine-input-late-acquire-1/logs/summary.json),
[implementation and offline checks](../../third_party/notproton/integration/wine-input/REPLAY-EXPERIMENT.md).

Multiple-controller ordering and physical delivery are not verified. Rebinding
the same source after disconnect is now verified with a software fixture. Wine's filters operate on VID/PID pairs, so the product refuses
an ambiguous pair rather than hiding another identical physical device. The
policy is covered offline; physical-device filtering still requires hardware.
No physical-device, rumble or multi-controller success is claimed.
[Filter evidence](../../outputs/steam-input-research/winebus-device-filtering.md).

The native UI reports actual API/identity/scan/controller/route results and
invalidates old reports after settings changes. The completed component and
transport checks do not justify labelling Steam Input support fully finished.

## Installed local candidate

The tested bundle was promoted to `dist/SkinOS.app`, preserving component hashes
and passing strict nested signature verification. The previous bundle is retained
for rollback. The installed native window opened with the authenticated library
and no manual login. Final product diagnostic saw a genuine mapped controller
but correctly reported no meaningful activity for its neutral software fixture;
normal Soundpad launch/stop passed with the updated bus. Hardware remains deferred.
[Installed artifact and scope](../../outputs/steam-input-installed.json),
[final product diagnostic](../../outputs/steam-input-product-fixture-final.json),
[final normal launch](../../outputs/game-input-soundpad-fixture-final.json),
[updated driver 32-bit conformance](../../outputs/wine-input-replay-di-x86-1/logs/summary.json).

## Reconnect build installed

The exact final reconnect candidate passed a second full normal launch/stop run
with an independent Windows observer, then was installed at `dist/SkinOS.app`.
The prior bundle is retained. Latest checks: 108 runtime tests, 14 native UI
Steam Input tests, strict Clippy and strict bundle signature validation.
[Installed artifact](../../outputs/steam-input-reconnect-installed.json),
[final observer readbacks](../../outputs/game-input-reconnect-final.json).

Standalone authenticated QA must run after closing the interactive SkinOS core.
Concurrent test cores displaced the old UI Steam session during development;
the saved credential remained usable on restart. The QA entry points now refuse
to start another authenticated core while one is running, before login or driver
setup. No credential or private-message content is read by these checks.
