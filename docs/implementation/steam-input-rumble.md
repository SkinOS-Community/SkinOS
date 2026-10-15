# Steam Input rumble: verified seams and implementation plan

Investigation and implementation record, 2026-10-14. The static-source findings
below preceded the isolated live checks recorded at the end of this document.
**The full Wine XInput → genuine Steam Input SDK → original SDL feedback path
has passed software checks, including retained effects and final zero. Ordinary
game feedback and reconnect have also passed.** No physical controller has been tested;
DirectInput PID force feedback is a separate capability.

The investigation sections below record how the implementation was derived.
Current one-controller results are [full retained callback proof](../../outputs/game-input-rumble-feedback-9.json),
[81 ordinary game requests](../../outputs/game-input-rumble-product-2.json),
[input and feedback reconnect](../../outputs/game-input-feedback-reconnect-2-runtime.json),
and [a normal game without a controller](../../outputs/game-input-feedback-no-controller-1.json).
The user's physical-device test remains deferred. Initial input preparation
still expects the selected controller before game launch; multiple controllers
and changing controller models during a running game are not supported.

The verified candidate is installed as `dist/SkinOS.app`; its previous build is
preserved. [Installed component receipt](../../outputs/input-feedback-installed.json)
and [native UI startup observation](../../outputs/input-feedback-installed-ui.json)
bind the tested payloads to the opened application. Its library opened with 232
games and Invisible status without a manual sign-in action. The separate
[connection recovery check](../../outputs/reconnect-check-feedback-final.json)
started retry after 10.001872 seconds and kept the running game alive.

## What the selected CrossOver actually does

The inspected Preview files are under
`/Applications/CrossOver Preview.app/Contents/SharedSupport/CrossOver/lib/wine`:

| File | SHA256 | Static entry / output implementation |
| --- | --- | --- |
| `x86_64-windows/xinput1_4.dll` | `c6236cf1cfffc7fe012644d60da06edf90a154e8bbb06547f65fc93359d7c048` | `XInputSetState` RVA `0x15f0` → `0x1800` |
| `aarch64-windows/xinput1_4.dll` (ARM64X native view) | `c6040b1d3e38336288651a5b900b7fdae79bb0233f245d132ae513c94357e367` | `XInputSetState` RVA `0x10678` → `0x10894` |

Both bounded disassemblies show the connected-controller path calling the
output routine and then returning zero independently of its result. The routine
caches the requested two motor values, skips disabled/unchanged requests,
populates Haptics page `0x0e`, Intensity usage `0x23`, and calls the imported
`HidD_SetOutputReport`. A failed write only reaches the logging path. On ARM64,
the output call is at RVA `0x109e4` through thunk `0x142b0`/IAT `0x30028`;
the public function resets its result to zero at `0x1072c`. On x64, the call is
at `0x1974` through thunk `0x5710`/IAT `0x7f48`, followed by unconditional public
success at `0x168d`. Therefore **`XInputSetState == 0` is not delivery proof**.
[Digested installed-binary evidence](../../outputs/steam-input-research/rumble-binary/manifest.json).

The pinned Valve Wine source explains the same general path:
`XInputSetState → HID_set_state → HidD_SetOutputReport → hidclass →
winexinput XI child → owned bus PDO`. Hidclass passes a `HID_XFER_PACKET`
through `IOCTL_HID_SET_OUTPUT_REPORT`; its buffer is not a raw byte pointer.
The IG compatibility child rejects these output/feature operations, so this is
an XInput two-motor seam, not a DirectInput force-feedback implementation.
[Full source ledger, exact framing and line links](../../outputs/steam-input-research/wine-rumble-source.md).

REA/Hopper was attempted for the small x64 function. Its first attempt rejected
the default launcher path; a scoped correct launcher then produced no result
within the bounded wait and was stopped. No completed REA decompilation is
claimed. The conclusions above use retained LLVM instructions and import
tables from the exact installed files. The ARM64X alternate EC export view,
other CrossOver releases, and complete installed HID/PnP internals were not
separately audited. [Attempt record](../../outputs/steam-input-research/rumble-binary/rea-attempt.json).

## Required owned HID surface

The original genuine producer descriptor remains exactly 188 bytes and declares
no Output/Feature reports. Its original `libsteaminput` export ABI has no rumble
callback: the previously audited macOS SetReport block ignores its arguments.
Keep that producer boundary intact. Add haptics only to the **owned canonical
Wine descriptor**, whose input representation is already adapted independently.
[Existing ABI evidence](../../crates/runtime-host/native/steam_input_output.h).

XInput discovers motors by an actual HID feature query. It needs two Haptics
Simple Controller collections containing Waveform List feature values and
matching Output Intensity fields. The waveform values are Rumble `0x1005` and
Buzz `0x1004`; arbitrary output bytes alone are insufficient. Both unsigned
16-bit intensity fields must share one output report ID. Reuse the first two
complete main-motor templates from pinned Wine's `hid_device_add_haptics`:

| Proposed two-motor surface | Bytes, with example report ID 1 |
| --- | --- |
| GET_FEATURE, 17 bytes | `01 05 10 00 00 e8 03 00 00 04 10 00 00 e8 03 00 00` |
| SET_OUTPUT, 5 bytes | `01 left_lo left_hi right_lo right_hi` |
| Explicit stop | `01 00 00 00 00` |

This subset is source-derived and still needs live descriptor validation. Use
separate Push/Pop report-ID state or an equivalent checked descriptor layout so
the current no-ID input report does not silently acquire a byte prefix. Parse
the resulting descriptor and verify all input offsets/lengths before use. For
nonzero output IDs, hidclass retains the ID in `reportBufferLen`; validate the
actual DDK packet type, ID, exact length, live child and current session before
accepting it. Implement truthful Feature reads; unsupported writes must return
an error. [Pinned descriptor and parser details](../../outputs/steam-input-research/wine-rumble-source.md#descriptor-discovery-arbitrary-output-bytes-are-insufficient).

## Genuine upstream sink and a test without hardware

Use the public `SteamInput006::TriggerVibration(handle, left, right)` API or its
flat export `SteamAPI_ISteamInput_TriggerVibration`. It takes the real Steam
Input handle and two 16-bit motor values; the return type is **void**, so a call
means only dispatch, not successful motor delivery. It may ignore unsupported
controllers. Revalidate account, AppID, connected handles and both directions
of the controller/gamepad-index mapping before nonzero dispatch.
[Valve API](https://partner.steamgames.com/doc/api/ISteamInput#TriggerVibration),
[pinned C ABI](https://github.com/ValveSoftware/Proton/blob/164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca/lsteamclient/steamworks_sdk_153a/steam_api_flat.h#L682).

The existing Windows probe already declares this method after genuine game-SDK
initialization; it is the smallest first experiment. The independent native
probe uses verified public flat APIs and can later become a retained control
session. The original Brotato `libsteam_api.dylib` also exports the required
v006 accessor, both mapping getters and TriggerVibration; its SHA256 is
`7824c030a7626ac1de9f1c6e03d45276721aa88ac419954b235cccf4546b15b3`.
Export presence does not prove invocation. A production sink must use an explicitly supplied/pinned SDK
payload or a separately audited public client connection, not an incidental
dependency on Brotato's installed files.
[Windows probe](../../third_party/notproton/integration/input-probe.cpp),
[native probe](../../crates/sdk-probe/src/input.rs),
[export evidence](../../outputs/steam-input-research/rumble-binary/native-sdk-exports.json).

For a software-only observation, extend the existing explicitly opted-in SDL
fixture with `SDL_VirtualJoystickDesc.Rumble` and stable `userdata`. The pinned
original SDL advertises rumble support when this callback exists, and its
virtual driver calls that callback for a real SDL rumble request. The callback
must only record bounded low/high values, sequence and time; it must not invoke
SDL_Rumble itself. Keep its state/library alive through detach and the SDK
consumer shutdown. Then call **the genuine Steam Input API**, observe whether
Valve reaches that original-SDL callback, and record the actual result. The
existing fixture leaves the callback null, so its current no-rumble capability
cannot establish this path.
[Pinned original SDL implementation](https://github.com/libsdl-org/SDL/blob/70e9cc86d/src/joystick/virtual/SDL_virtualjoystick.c#L560-L584),
[fixture](../../crates/runtime-host/src/input_fixture.rs).

First test sequence: verify the exact fixture handle/slot; command left-only,
zero, right-only, zero with short bounded intervals; require ordered callbacks
and a final zero. Preserve observed scaling rather than assuming Valve's rumble
settings leave amplitude unchanged. Re-run with the wrong/retired generation
at our dispatcher boundary and require rejection without any SDK call. A
successful callback proves **Valve command routing to the software device**;
physical vibration remains unverified. This is the next implementation step
before changing the Wine driver's advertised output capability.

## Minimal return channel

Proposed modules and obligations, not existing implementation:

1. `wine-input/driver.c`: on a valid child Output packet, queue a bounded
   feedback record containing protocol version, control-session identity,
   child slot, child generation, monotonic sequence and left/right values.
   The actual PDO supplies identity; never route from a guessed XInput index.
   Return the real queue acceptance/error status. Cancelled/removed IRPs and
   a full queue cannot become successful acknowledgments.
2. `feeder.c`: dequeue feedback asynchronously through the same owned control
   handle, serialize it as a separate typed event, and retain it until the
   runtime acknowledges the matching sequence. Do not mix it with input-frame
   ACKs or let two stdout writers interleave. On EOF, cancellation or teardown,
   stop feedback collection and ensure the runtime sees session termination.
3. `wine_input_relay.rs`: validate a new feedback event schema and bounded queue;
   current `accept_status` deliberately rejects unknown events. Tag every event
   with the parent-owned connection nonce/session epoch. Forward it to the
   worker thread rather than calling Valve from the relay's IO thread.
4. `worker/wine_input_session.rs` plus a small retained SDK sink: maintain
   `(connection epoch, control session, slot, PDO generation) → genuine handle`.
   Dispatch on the SDK-owning thread after revalidation. Record separately
   `driver_accepted`, `sdk_dispatched` and `fixture_callback_observed`; the SDK's
   void return cannot be converted into `motor_delivered=true`.
5. Clear retained nonzero state on explicit zero, destroyed generation, focus
   loss, game exit, relay error, sequence gap or shutdown. Send zero to the old
   handle while it is still valid, never to a replacement handle. Do not wait
   for an XInput-generated zero after abrupt process/device loss.

There is a duration trap: stock Wine defaults its backend effect cutoff to
1000 ms, while XInput caches the requested nonzero speeds and suppresses equal
requests; the source contains no periodic repeat in that path. A new sink must
explicitly choose/document its retained-request and downstream-refresh policy.
Do not infer an infinite effect from duration zero, or silently expire a motor
then claim the retained request is still fulfilled. The initial short
left/zero/right/zero proof avoids making that unverified duration claim. Once
the genuine sink's duration is observed, maintain a bounded active-session
refresh if needed for XInput's retained desired state and always stop on loss.
[Duration/source evidence](../../outputs/steam-input-research/wine-rumble-source.md#duration-repeat-and-explicit-stop).

## Multiple pads: identity precedes index

Wine's ordinary XInput index is process-local and may differ from the Valve
output slot. The pinned Proton source has a special override for a specific
Valve VID/PID/path; its literal was absent from the installed CrossOver files,
which is only a bounded negative check, not proof of their whole slot policy.
Do not rename our device to impersonate that special case.
[Source assignment](../../outputs/steam-input-research/wine-rumble-source.md#minimal-own-bus-requirements-and-controller-identity),
[installed literal check](../../outputs/steam-input-research/crossover-xinput-slot-literal-check.json).

Feedback can avoid index ambiguity because the output IRP identifies its own
PDO. For forward-game mapping and test attribution, still verify two distinct
owned paths, genuine handles and the observed process-local XInput table. The
next multi-pad test must create slots in reverse order, use distinct states,
remove/recreate one generation and confirm rumble reaches only the requested
original fixture. No multi-pad, physical-device, trigger-rumble or DirectInput
PID force-feedback success is claimed by this investigation.

## Verified sink milestone

The four-command real-SDK sequence reached the original SDL fixture callback
with exact requested values and final zero. Parent route grounding, reciprocal
SDK mapping, SDL cleanup, SDK/relay/engine teardown and absence of original Steam
GUI all passed. No physical-controller or full Wine-feedback success is implied.
[Live proof](../../outputs/steam-input-rumble-sink-milestone.json),
[integration and limits](steam-input-rumble-qa.md#verified-live-sink).

## Verified Wine transport and retained-connection diagnosis

The isolated Wine test confirmed actual `XInputSetState` requests reaching the
owned driver's output queue, including six motor pairs, generation retirement,
unchanged input checks and root/service cleanup. The descriptor's input/output/
feature sizes were 15/5/17 bytes. This is a transport proof, not proof that the
retained SDK connection consumed those records.
[Transport result](../../outputs/wine-rumble-transport-1/logs/summary.json).

The second combined Soundpad run reached genuine SDK initialization and a
verified parent SDL/Valve/output route, but stopped before the first Bind ACK.
Its inherited Wine stdin rejected `PeekNamedPipe` with Win32 error 50
(`ERROR_NOT_SUPPORTED`). The helper reported real Input/SDK shutdown; the
parent observed its kernel termination, stopped the game, drained the engine
and restored the original game settings. The next change is confined to that
stdin reader; no controller-routing relaxation follows from this failure.
[Captured diagnostic](../../outputs/steam-input-rumble-feedback-2-diagnostic.json).

The sixth combined run reached all four actual Windows XInput requests, matching
driver sequences 1–4, genuine SDK dispatch and original SDL callbacks with exact
values `16384/0 → 0/0 → 0/24576 → 0/0`. Soundpad reached running and exited;
the external driver and core exited with code 0, settings were restored, and
original Steam GUI monitoring was complete with none observed. XInput warmup
took 905 ms before readiness; the subsequent Bind ACK took 10 ms.

The run is **not an overall pass**: while closing, its process tracker reported
an unavailable executable identity before later observing kernel ESRCH. Actual
SDK/Input shutdown and SDL cleanup were recorded. The remaining correction is
a bounded wait for verified termination during closing, without treating an
unknown process inspection as exit.
[Complete callback evidence and remaining failure](../../outputs/steam-input-rumble-feedback-6-callback-proof.json).
