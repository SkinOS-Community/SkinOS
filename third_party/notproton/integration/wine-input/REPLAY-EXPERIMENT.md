# Cached-report replay experiment

The driver now refreshes a pending HID read from its latest unchanged mapped
report every 16 ms. This closes the specific late-acquire gap: Valve may emit
only changed input, while Wine DirectInput resets a newly acquired object's
state and waits for `ReadFile` rather than using the bus's `GET_INPUT_REPORT`.
This note supersedes the earlier read-only replay design's implementation status.

The worker never generates a producer frame or changes the source-frame count.
Fresh producer reports still complete pending reads immediately. Replay copies
the current canonical cache into claimed IRPs while holding the cancel spinlock,
then completes them outside all driver locks. It does not call `publish_report`,
write either report cache, or set `has_report` when no read is waiting. Consequently
a replay cannot overwrite a newer release with an older local snapshot.

One `PsCreateSystemThread` belongs to the driver. Each tick references current
PDOs under `devices_lock`, only while the root and producer session remain active;
removed children are checked again under the cancel lock. Slot replacement cannot
reuse an old referenced object's storage. Stop/cleanup retains the existing
session generation and cancellation rules. Unload sets the stop flag, signals a
notification event and joins the worker before deleting resources. It does not
rely on Wine's stub `KeFlushQueuedDpcs`. Failed creation rolls back DriverEntry;
unexpected worker wait failure retires devices and rejects new producer work.

## Offline verification

From the repository root:

```sh
python3 scripts/test-wine-input-replay.py
python3 scripts/build-wine-input.py --output-dir target/wine-input-replay-experiment
```

The first command extracts the actual queue/publish/replay and worker start/stop
functions, compiling them against a minimal instrumented host DDK shim with
ASan/UBSan. Ten cases cover unchanged caches, exact report bytes, absent/removed
devices, bounded reads, cancellation without double completion, completions
outside locks, thread creation failure, and join before handle close. The race
case injects the actual `publish_report(new_release)` after replay releases its
lock but before completing its IRPs; the next actual `queue_read` must return
the newer release. This is an offline lifecycle/queue check, not a kernel or
end-to-end Steam Input proof.

The second command builds ARM64 and x64 SYS/feeder/consumer candidates and an
x86 consumer in the experiment directory only. It never installs or starts Wine.
Keep `target/wine-input`, bundled payloads and game prefixes unchanged until live
conformance passes and the caller explicitly promotes the candidate manifest.

## Controlled live late-acquire test

The parent test runner owns all Wine execution and installation. Use a **new,
owned prefix**, the candidate architecture that matches `winedevice`, verified
manifest bytes, and the established automatic prefix bootstrap. Preserve exact
loader spelling and keep a foreground helper alive across install/activation.
An existing keeper consumer may remain alive; each late-acquire consumer below
must be a newly started process with a new DirectInput object.

1. Open the feeder, create one owned slot and wait for genuine bus readiness.
   Send **exactly one** non-neutral report: A held, LT 255, LX 12345, LY -23456.
   Record the feeder's single report acknowledgment and the producer/report
   count. Do not send another report while performing steps 2 and 3.
2. Only after that acknowledgment, start a new DirectInput consumer. Verify the
   complete owned `DIPROP_GUIDANDPATH` slot/generation, not product name or first
   global controller. Poll within a bounded two-second window for the exact
   held button, independent XInput axes/triggers and documented DirectInput
   transformed values. The first poll may precede the next 16 ms refresh; this
   is not permission to accept a permanently neutral value.
3. Start a second fresh consumer and require the same values, still with only
   one producer report. Preserve the first consumer and verify it remains
   consistent. Record PID/start times, the source report count and both readbacks
   so an accidental duplicate producer report cannot satisfy the test.
4. Send one neutral release. Both consumers must reach strict released state
   and stay released for several refresh periods. A stale held completion must
   not overwrite the stored new report or persist as consumer state.
5. Send one held report, then close feeder stdin. Both consumers must observe
   owned interfaces disappear; pending reads must fail/close. Reopen/create and
   require a new generation with initial neutral state, without old held data.
6. Remove the owned root/service with pending readers and verify unload finishes.
   Reinstall/open once more to exercise joined-worker teardown and recreation.
   Finish with bounded readback proving root and service absence.

Run existing 54-case XInput and 59-case DirectInput conformance alongside this
new case, including the x86 DirectInput consumer on the ARM64 FEX device host.
Measure idle CPU with no child, with an acquired idle child and with a held child.
Neither fixture input nor periodic replay is evidence of physical-controller
delivery, Steam Input mapping correctness, rumble or multiple-controller support.
Live replay acceptance remains pending until the parent records these results.
