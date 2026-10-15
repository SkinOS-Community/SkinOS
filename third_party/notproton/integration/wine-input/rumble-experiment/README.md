# Isolated Wine HID rumble feedback experiment

This directory contains copied owned transport sources. The production driver,
feeder, protocol and canonical input descriptor are untouched; their copy-time
hashes are in `base-provenance.json`. Builds write only
`target/wine-input-rumble-experiment`. No native SDK dispatch is implemented here.

## Descriptor gate

The original producer descriptor188/report20 remain unchanged. The canonical
input items also remain unchanged. `adapter.h` appends two Haptics Simple
Controller collections inside the existing application collection, isolated by
HID Push/Pop, with Output/Feature Report ID 1. Input retains ID 0 and 14 payload
bytes (15 at the public HID API because of its zero prefix). Output is 5 bytes,
Feature is 17 bytes. Do not change these input framing rules when adding output.

`scripts/test-wine-rumble-descriptor.py` compiles the **unchanged pinned Wine
descriptor parser and HidP implementation**, plus the unchanged XInput waveform
selection function. Its actual parsed internal/public input caps are byte-for-
byte equal to the baseline. It resolves the Rumble and Buzz collections and
checks 25 motor pairs produce `[1,leftLE16,rightLE16]`. Host allocation/feature
I/O are explicitly mocked; this is parser conformance, not live motor delivery.
See `parser-README.md` and
`target/wine-rumble-descriptor-parser-test/report.json` for source hashes and
the complete field-offset table.

## Feedback and binding contracts

Output `IOCTL_HID_SET_OUTPUT_REPORT`/`WRITE_REPORT` validates the actual
`HID_XFER_PACKET`, exact ID/length and current own PDO. Accepted motor values
enter a 64-record queue under the device/session lock. Queue-full, cancellation,
removed child or wrong session returns an error; it never acknowledges an
overflowed command. There is no pending feedback IRP: dequeue is a bounded,
immediate poll. Existing asynchronous **input** read/cancellation logic remains
inherited from the copied driver.

`0x0022a008` (METHOD_BUFFERED, FILE_WRITE_ACCESS) takes no input and returns
exactly 48 bytes: twelve little-endian u32 fields:

```text
magic=0x53495242, version=1, kind=1, byte_size=48,
control_session, slot, PDO_generation, sequence_low, sequence_high,
left_motor, right_motor, report_id=1
```

The motor fields must fit 16 bits. Sequence advances only for accepted HID
outputs, never on overflow. Empty dequeue returns STATUS_NO_MORE_ENTRIES.
Destroy removes queued records for exactly that slot/generation; control close
and a new owner clear the whole queue. A previously accepted in-flight read can
complete at the teardown boundary, so a future runtime receiver must still
validate its own connection epoch and current generation before dispatch.

`0x0022a00c` takes one LE u32 slot and returns 24 bytes:

```text
magic=0x53495242, version=1, byte_size=24,
control_session, slot, PDO_generation
```

The driver snapshots this binding only for the current owner and active child.
Experimental feedback mode queries it immediately after Create and includes
`control_session`, `generation`, `driver_binding_verified:true` in the normal
`wine_input_ack`. A downstream sink can therefore establish its authoritative
generation before any feedback arrives.

`--ensure-feedback <INF>` installs/verifies/activates and retains one exclusive
producer handle; `--feedback-feed` uses an already prepared own driver. They
accept the existing input frames unchanged, plus an experimental stdin control
frame `kind=4, slot=0, payload_len=0` to poll feedback on that **same handle**.
The serial feeder emits `wine_input_feedback` with `available:false`, or the
dequeued fields and `hid_output_dequeued:true`. No second writer thread, extra
producer handle, SDK call or invented acknowledgment is introduced. Normal
`--feed` continues to reject the new control command.

The fixed Feature response describes two waveform entries and default cutoff
metadata; this experiment does not start any actuator or enforce an effect
duration. A later genuine SDK sink must define its own verified refresh/stop
lifecycle. XInput's successful return and desired-speed cache do not establish
actual output delivery.

## Independent Windows consumer

`rumble-consumer.c` calls public SetupAPI/HidD/HidP, DirectInput and the actual
installed `XInputSetState`. It requires the otherwise-empty single own pad,
matching full XI/IG paths, slot 0, a nonzero same generation, actual caps15/5/17,
Feature17 bytes and correct haptics collections. It pins the first successful
generation; a replacement requires a fresh consumer process. It never routes an
old pending stop to a replacement. Its successful API result always carries
`delivery_verified:false`: only the parent can join that result to a real
driver feedback record.

stdin frames use the same 288-byte framing, with `kind=6,payload_len=0` for
inspection and `kind=5,payload_len=4` for two LE u16 motors. These are consumer
commands, not mapped input reports. Replies are `wine_rumble_result` with
actual path/generation/caps, requested motors, and XInput return code.

## Build and parent-owned live test

```sh
python3 scripts/test-wine-rumble-descriptor.py
python3 scripts/build-wine-input-rumble.py
clang -std=c11 -O1 -g -Wall -Wextra -Werror -fsanitize=address,undefined \
  third_party/notproton/integration/wine-input/rumble-experiment/feedback-test.c \
  -o target/wine-input-rumble-experiment/feedback-test
target/wine-input-rumble-experiment/feedback-test
```

All target files are manifest-hashed. The live harness refuses existing prefixes
or nonempty logs, verifies all payload bytes, initializes a new prefix once,
and runs verified copies from `C:\SkinOSRumbleQA`. Root supplies the exact owned
Wine loader and matching driver architecture:

```sh
python3 scripts/check-wine-rumble.py \
  --wine "$HOME/Library/Application Support/SkinOs/notproton/runners/crossover-27.0.0.41069-fex/CrossOver/skinos-launcher/aarch64-unix/Wine.app/Contents/MacOS/wine" \
  --arch arm64 --consumer-arch arm64 \
  --prefix outputs/wine-rumble-transport-1/prefix \
  --log-dir outputs/wine-rumble-transport-1/logs
```

The harness runs the existing input matrix, then left-only/zero/right-only/zero/
both/zero through real XInput. Each request must produce an actual dequeued HID
record with matching motors, authoritative session, PDO generation and increasing
sequence. An API return of zero without that record fails. It also checks queued
old-generation outputs disappear after Destroy, uses a fresh consumer to inspect
the replacement generation, and confirms root/service removal during cleanup.
`summary.json` records both API requests and driver observations independently.

No Wine or physical-controller execution was performed while preparing these
files. Until the parent records live success, this remains an isolated build
and offline-conformance result. It does not prove SDK rumble delivery, physical
vibration, multiple pads, hotplug routing or DirectInput PID force feedback.
