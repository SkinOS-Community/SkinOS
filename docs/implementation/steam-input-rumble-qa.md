# Isolated genuine Steam Input rumble sink QA

Integrated and live-verified 2026-10-14; **strictly opt-in software-fixture QA**.
This tests the SDK-to-original-SDL sink only. It adds no Wine HID capabilities
and changes no input/feedback protocol.

## Files and offline checks

- `crates/runtime-host/src/input_fixture_rumble.rs`: strict opt-in registration,
  bounded callback recorder, separate SDL-owned Arc lifetime, closed observation
  window/rundown, ordered left/zero/right/zero proof and detach monitor.
- `third_party/notproton/integration/input-rumble-qa.h`: real public SteamInput006
  API sequence; no direct SDL_Rumble call. Every phase checks account, AppID,
  unique handle, reciprocal gamepad mapping, one connected XInput pad and neutral
  state. Input SDK initialization is mandatory.
- `scripts/build-input-rumble-qa.py`: creates a candidate copy of the existing
  probe and its small integration diff; the original file is never rewritten.
- `scripts/test-input-rumble-qa.py`: actual-header host mocks; these are policy
  tests, not successful Steam SDK or physical-controller evidence.

```sh
mkdir -p target/input-rumble-qa
rustc --edition=2024 --test crates/runtime-host/src/input_fixture_rumble.rs \
  -o target/input-rumble-qa/fixture-rumble-tests
target/input-rumble-qa/fixture-rumble-tests
python3 scripts/test-input-rumble-qa.py
python3 scripts/build-input-rumble-qa.py
```

Verified offline: 10 Rust tests and 14 ASan/UBSan C++ cases passed; candidate
x64/x86 PE probes linked with `-Werror`. Artifact hashes are in
[`target/input-rumble-qa/manifest.json`](../../target/input-rumble-qa/manifest.json).
No production target, app bundle, DLL, Wine prefix or account was changed.

## Host integration window

Root owns this integration after the current hotplug run finishes. Preserve
the existing consuming `Fixture::detach_for_reconnect_test` and Drop behavior.

1. Register `mod input_fixture_rumble;` in the runtime-host root and add
   `rumble: Option<crate::input_fixture_rumble::Registration>` to `Fixture`.
   Inside its existing `attach`, **after** the pinned original SDL version,
   revision and initialized-subsystem checks, replace only descriptor setup:

   ```rust
   let mut rumble = crate::input_fixture_rumble::Registration::from_environment()?;
   let mut desc = neutral_description();
   if let Some(registration) = &rumble {
       let callbacks = registration.callbacks();
       desc.userdata = callbacks.userdata;
       desc.rumble = Some(callbacks.rumble);
       desc.cleanup = Some(callbacks.cleanup);
   }
   let joystick_id = attach(&desc);
   if let Some(registration) = &mut rumble {
       registration.finish_attach(joystick_id)?;
   }
   // Keep the existing joystick_id == 0 check; store `rumble` in Fixture.
   ```

   `finish_attach` must immediately follow the synchronous FFI call, before
   any fallible work. Early allocation failure never copied userdata; partial
   attachment may already have called Cleanup. The shared release flag avoids
   freeing the SDL reference twice. Successful attachment keeps that reference
   until Cleanup, even if the Rust owner is dropped or a detach attempt fails.
   [Pinned SDL cleanup and attach](https://github.com/libsdl-org/SDL/blob/70e9cc86d/src/joystick/virtual/SDL_virtualjoystick.c#L57-L146),
   [synchronous detach](https://github.com/libsdl-org/SDL/blob/70e9cc86d/src/joystick/virtual/SDL_virtualjoystick.c#L312-L320).

2. Enable only for an explicit owned-software-fixture diagnostic, with
   `SKINOS_INPUT_RUMBLE_TEST=1` and `SKINOS_STEAM_INPUT_FIXTURE=neutral`.
   Reject a combined activity or reconnect experiment: this sequence requires
   the fixture to remain neutral and attached. Before spawning the probe, prove
   that the actual registered SDL fixture is the sole intended genuine
   controller, using the current fixture log/config/readiness checks. Retain
   `registration.monitor()` and call
   `registration.begin_observation(fixture.joystick_id)`.

   **The host must bind the owned fixture identity to that unique genuine
   route.** The helper receives `SKINOS_INPUT_RUMBLE_FIXTURE_ID=<actual id>` as
   parent metadata; a numeric environment value does not independently prove
   which hardware a Steam handle represents. Do not set these options for a
   normal game, physical-controller scan, failed fixture attachment or ambiguous
   device inventory. The helper reports `fixture_id_from_parent`, never an
   independently verified fixture identity.

3. For the isolated run select the matching candidate probe from
   `target/input-rumble-qa/pe`, verifying its manifest first. Its generated
   [`input-probe-integration.patch`](../../target/input-rumble-qa/input-probe-integration.patch)
   adds a cleanup hook, the new header and a call after the ordinary real SDK
   scan. It does not alter SDK init, authentication or existing input metrics.
   Keep the normal host pump active. The extra rumble sequence takes about
   two seconds, with a five-second internal deadline and a route check every
   25 ms. Normal probe timeout must include that extra bounded interval.

4. Validate the actual stdout transcript: a real `steam_input_rumble_route`
   whose AppID, handle and slot agree with the preceding SDK samples; exactly
   four `steam_input_rumble_dispatch` rows for `(16384,0)`, `(0,0)`, `(0,24576)`,
   `(0,0)` in phase order 0..3; a commands-complete row; successful process exit.
   The helper never claims callback delivery: `callback_verified` and
   `physical_device_verified` remain false. On error, its cleanup attempts zero
   only on the old still-connected genuine handle under the same account/AppID.
   It never substitutes a newly connected handle.

5. After the probe exits, call `registration.ordered_readback()`. This closes
   the observation window before snapshotting; if a callback is still executing,
   keep pumping and retry within a short bounded deadline, e.g. 250 ms. Do not
   re-open the recording window or ignore lost/overflowed callbacks. The result
   retains actual observed low/high magnitudes, which need not equal requested
   values if Valve scales intensity. It requires left-only, zero, right-only,
   final zero; repeated equal callbacks and initial zeros are allowed.

6. Tear down the own fixture through its existing path. The separately retained
   monitor must report `cleanup_seen() == true` after successful pinned SDL
   detach. Serialize the callback samples, SDK dispatch transcript and cleanup
   result separately. Claim a passed software sink test only when **all** gates
   pass; never convert the void TriggerVibration call or fixture callback's bool
   into proof that a physical motor moved.

The SDK helper's first default-off invocation does nothing. The Rust callback
only records within the explicitly opened observation window. A live callback
capability must not be published for other SDL devices. Current software tests
do not justify physical rumble, controller-type coverage, impulse triggers,
DirectInput force feedback, or the complete Wine-game feedback route.

## Verified live sink

The packaged candidate passed the actual `diagnose_input` path with
`--neutral --rumble --require-controller --require-xinput` for Raft AppID648800.
Parent-side fresh SDL/log/config/index/slot and output-ACK checks preceded the
real SDK probe. The four requested pairs were observed unchanged in the original
SDL fixture callbacks: `(16384,0) → (0,0) → (0,24576) → (0,0)`. The actual account,
AppID and reciprocal Steam Input handle/XInput slot matched. SDL Cleanup, SDK
shim exit, engine shutdown and relay drain were confirmed before publication.
No original Steam GUI was observed. This verifies the genuine Steam Input sink;
physical motors and the Wine XInput feedback channel are separate, unfinished
verification scopes.

[Live transcript](../../outputs/steam-input-rumble-sink-1.json),
[artifact hashes and lifecycle proof](../../outputs/steam-input-rumble-sink-milestone.json).
The normal product diagnostic remains silent; only the explicit fixture flag
can dispatch this sequence. Both probe architectures now have a bounded sibling
SHA-256 manifest validated before this QA path. Current verification:123 runtime
tests,15 actual-header policy cases with ASan/UBSan, strict Clippy, x64/x86 builds.
