# Retained Steam Input rumble sink

Updated 2026-10-14. **Selected by ordinary per-game Steam Input.**
Its build does not run Wine or an SDK session or change the production probe.
The complete feedback path reached original SDL callbacks with three-second
retained effects and final zero; ordinary game dispatch and same-model reconnect
also passed. [Current proofs and limits](steam-input-rumble.md).

Before readiness, four read-only XInput calls warm its initial device inventory.
A new Bind allows up to 500 ms for an unambiguous SDK route to settle after
hotplug, while pumping callbacks/RunFrame and retaining identity checks. The
750 ms command ACK deadline starts only after the complete command was written.
In the verified reconnect the SDK route needed 102 ms to settle; parent-only
readiness was insufficient.

## Build and policy tests

```sh
python3 scripts/test-input-rumble-sink.py
python3 scripts/build-input-rumble-sink.py
```

- `third_party/notproton/integration/input-rumble-sink.h` is the actual candidate
  implementation exercised by the native Win32/SDK mocks with ASan/UBSan.
- The builder checks exact integration boundaries, transforms a copy of the
  current `input-probe.cpp`, and adds only the required imports to an isolated
  copy of its PE builder. Output is `target/input-rumble-sink-experiment`.
- Its manifest records original/candidate hashes and `executed:false`; matching
  x64 and x86 probes are built with `-Werror`.
- The entry gate is exactly `SKINOS_INPUT_FEEDBACK_SINK=1`. Absent/empty leaves
  the ordinary candidate scan unchanged; other values fail before vibration.
- A real game SDK, verified account/AppID, initialized SteamInput006 and XInput
  availability are mandatory. A missing game SDK fails explicitly. There is no
  fallback to an unrelated game's DLL and no fabricated public interface.

The retained mode enters after the existing genuine context initialization and
before its ordinary scan. It only calls read-only discovery, RunFrame and genuine
TriggerVibration; it does not activate action sets or write a layout. Physical
button/stick activity is allowed and is never a targeting heuristic.

## Control protocol v1

One command is exactly **64 bytes**, sixteen little-endian u32 words. There is
no text parser, allocation from incoming lengths, or variable payload.

| Offset | Field | Rule |
| --- | --- | --- |
| 0 | magic | `0x53495253` |
| 4 | version | `1` |
| 8 | byte_size | `64` |
| 12 | kind | Bind1, Submit2, Retire3, Ping4, Shutdown5 |
| 16,20 | parent epoch low/high | nonzero u64, fixed after first Bind |
| 24 | driver control_session | nonzero, fixed after first Bind |
| 28 | output slot | exactly 0 in current one-controller scope |
| 32 | PDO generation | bound generation;0 while unbound |
| 36,40 | command sequence low/high | starts 1, strictly previous+1; no wrap |
| 44,48 | left/right motors | u16 range; nonzero allowed only in Submit |
| 52,56,60 | reserved | all 0 |

This sequence is **the sink-command ordinal**, including Ping/Retire/Bind, not
necessarily the driver's feedback sequence. The Rust owner retains the
association between a feedback record and the command it submits.

Bind accepts only an unbound state and a generation higher than any previously
bound generation in this control session. It discovers the actual nonzero
Steam Input handle, requires exactly one controller, both mapping directions,
and exactly one connected XInput slot 0. The parent must independently prove its
SDL-to-Valve-to-output route and authoritative driver Create ACK binding; these
numeric command fields do not prove PDO provenance by themselves.

Submit requires the exact current epoch/session/generation, real logged-on
account/AppID and unchanged reciprocal route. It calls TriggerVibration on the
retained genuine handle, retains the latest desired motors, and emits a typed
ACK with `sdk_dispatched:true`. `desired_sequence` identifies the Submit that
owns the retained value, separately from the latest command sequence.

Retire attempts zero only on the old handle while still connected under the
same account/AppID, acknowledges what was actually dispatched, then clears
handle/generation/motors. Subsequent Ping/Shutdown uses generation 0. It never
redirects zero to a replacement handle. A new Bind cannot reuse a retired PDO
generation. A new control session requires a new helper/parent epoch.

Ping renews a **1500ms** heartbeat lease after checking identity and any active
route. A lost route suspends the sink: it clears the old desired motors, stops
refresh and rejects in-flight old Submit commands without dispatch. Retire and
Shutdown remain available. Only a verified Retire followed by a fresh higher-
generation Bind can resume output; the parent also requires a fresh source route.
Submit does not renew the heartbeat lease. Before initial Bind the timeout is 5s.
When bound and nonzero, the helper re-dispatches the retained desired speeds
at **250ms** intervals while the lease, identity and route remain valid.
Refresh rows are separate from command ACKs. This is an explicit retained-value
policy for XInput's cached equal requests. The original SDL fixture retained
each channel for three seconds with no intervening XInput request; physical
motor duration remains unverified.

Shutdown attempts old-handle zero, calls real Input Shutdown and SDK Shutdown,
and emits `steam_input_sink_stopped` carrying epoch/session/sequence, actual
Input Shutdown result and whether zero/SDK shutdown were dispatched. EOF uses
the same teardown. Missing heartbeat, identity loss, malformed/replayed
commands, parser limits or writer failure stop the sink and return failure.
All reports keep `physical_device_verified:false`.

## Thread and resource boundaries

Only the main thread calls genuine SDK APIs. A dedicated reader thread uses
blocking ReadFile and a bounded 4096-byte ring; the SDK thread dequeues without
waiting, pumps at 5ms intervals and processes at most 8 input chunks per
iteration. A partial frame has a 250ms deadline. Queue overflow or more than
1024 commands per second is rejected. EOF is observed only after queued bytes
are drained. Inherited Wine stdin returned ERROR_NOT_SUPPORTED (50) for
PeekNamedPipe in the earlier live candidate; that API is no longer imported.
A blocked reader does not prevent lease expiry or SDK shutdown. Its owned thread
handle is closed without waiting, and helper ExitProcess ends the pending read.

ACKs use a bounded 32-line queue, each at most 512 bytes. A dedicated writer
thread performs only WriteFile. It never holds the queue lock during the write;
the SDK thread never waits for stdout I/O. This avoids relying on named-pipe
mode support for inherited Wine stdio: synchronous pipe writes can otherwise
wait for the reader. [Microsoft pipe operation documentation](https://learn.microsoft.com/en-us/windows/win32/ipc/anonymous-pipe-operations).
Queue/lock exhaustion is an error, not a dropped success ACK.

After SDK teardown, final output draining is bounded to 200ms. A blocked writer
is ended by ExitProcess in this owned helper; an undrained/error response path
returns nonzero. The parent must track this real helper PID separately from
its waiting NotProton shim, stop it before host/SDK teardown, and reap the
owned shim after the input feeder closes. No prefix-wide kill is needed.

The product owner reports rumble failure independently of input/game failure,
revokes old bindings on disconnect/focus loss, and retains only the last 32
diagnostic requests without limiting session duration or the number of commands.
An ACK proves dispatch, never physical vibration. A no-controller session uses
the input owner's verified zero-device cleanup instead of inventing a relay or
SDK process that was never started.
