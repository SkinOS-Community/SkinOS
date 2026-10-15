# Owned Wine input transport

This is a Wine-only output transport for **already mapped** genuine Valve
`SteamInput_Send` reports. It does not implement Steam authentication, fabricate
Steam Input handles, or read a physical controller. The isolated Wine transport
has passed XInput and DirectInput conformance, including an x86 consumer. The new
automatic ensure path below has only offline verification so far; its caller
integration and genuine producer-to-game proof are separate work.

Build without running Wine:

```sh
python3 scripts/build-wine-input.py
```

Outputs: `target/wine-input/{arm64,x64}/{skinosinput.sys,skinosinput.inf,feeder.exe,consumer.exe}`,
plus `x86/consumer.exe`. PE drivers must match the **Wine device-service**
architecture, not merely the game's architecture. Compilation uses pinned Wine
headers and import libraries derived from the selected installed CrossOver;
`--wine-root` changes that source. All source is supplied here, with Wine-derived
portions under the accompanying `LICENSE.Wine`. The driver topology follows
[Wine's own test bus](https://github.com/ValveSoftware/wine/blob/290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c/dlls/dinput/tests/driver_bus.c).

Installation is explicit `feeder.exe --register Z:\\...\\skinosinput.inf`, followed
after that process exits by `feeder.exe --activate`. Registration success only
means SetupAPI accepted registration. Activation independently verifies the
owned root/service/binary, starts the service, requests Wine root enumeration,
then reads actual AddDevice/START state through the driver's status IOCTL.
`--install` remains a combined diagnostic entry, but conformance uses the two
separate processes. `--status` reports SCM/CM availability honestly; Wine's CM
stub does not establish PnP state. Actual StartService1055 permits bounded
retries (three attempts/15s), without acquiring or releasing any SCM lock.
It registers `ROOT\SKINOSINPUTBUS\0000` using SetupAPI and installs the owned
demand-start service. `--uninstall` checks our hardware ID/service before removal.
Both refuse native Windows; the intended environment is an explicitly created,
disposable Wine prefix. No CrossOver application files are overwritten.

## Automatic launch helper

`feeder.exe --ensure-and-feed Z:\\...\\skinosinput.inf` performs owned-driver
preparation and then remains the feeder in **one foreground Wine process**.
There is no gap between preparation and the input session requiring a separate
keeper. The existing `--feed` still only opens an installed driver.

The caller must first prepare the correct game prefix through NotProton, verify
the manifest SHA of feeder/SYS/INF and select the helper matching the real
`winedevice.exe` architecture. The helper additionally validates the sibling SYS
as a bounded PE of its own architecture and compares installed SYS bytes against
that payload. It holds a prefix-scoped named mutex until EOF/cleanup; other
mutating helper modes also respect the fence. `--status` stays read-only.

A matching owned installation is reused without re-registering it. A different
owned version is removed/stopped and replaced only after exclusive ownership is
available; an active owner fails with sharing violation before any update. The
old root and service must disappear before re-registration, and the installed
bytes are checked again before activation. Foreign ownership, an orphan different
SYS without an owned root/service, architecture mismatch or pending removal is
an error, never permission to overwrite another installation. No wineserver kill
or installed CrossOver modification is used.

Preparation JSON is written to **stderr**. stdout preserves the relay protocol:
one verified `wine_input_root_readiness`, then `wine_input_ready`, acknowledgments,
cleanup, closed and command-complete. If preparation fails, stdout contains a
failed command-complete event with the actual Win32 error and no ready event.
The caller should budget preparation separately from the old10-second `--feed`
startup timeout: actual service startup has bounded retry and readiness waits.
EOF removes this session's devices while leaving its owned installation for reuse.

`feeder-ensure-test.c` is a host-only mock lifecycle test: it verifies that busy
and foreign states cannot mutate, matching installations skip registration,
replacement orders remove/register/verify/activate/feed, and every failed stage
prevents later calls. These tests do not execute Windows APIs or establish live
installation/input success. Candidate builds can be isolated with
`--output-dir target/wine-input-ensure-experiment`; current live artifacts need
not be replaced to compile or validate the candidate manifest.

## Producer contract

`feeder.exe --feed` reads stdin as exact 288-byte frames: eight little-endian
u32 fields followed by payload[256]. Fields are magic `0x53494e50`, version1,
kind1=create/2=report/3=destroy, slot0–15, payload length, vendor, product, version.
Create carries the original 188-byte descriptor, verified byte-for-byte. Report
carries the original 20-byte mapped report. Destroy has length0. Invalid lengths,
truncated frames, duplicate creates and report-before-create fail explicitly.
stdout is JSONL with acknowledged IOCTL status. An acknowledgment means the
driver accepted the frame, **not** that a game received it. EOF removes the
feeder's children; driver control-handle cleanup also handles process death.

The driver publishes `WINEBUS\WINE_COMP_XINPUT` children and reuses existing
Wine `winexinput`, `winehid` and `hidclass` drivers. The original188/20 stay intact
at the input boundary. `adapter.h` changes only their representation to Wine's
canonical HID usages: buttons1–10, vendor-page Guide, hat0–8, Z/Rz triggers and
signed X/Y/Rx/Ry. Conflicting D-pad opposites cancel on each axis. Padding and
reserved bit11 never become input. XInput's reader keeps signed Y polarity;
Wine's DirectInput child performs its documented source-level inversion and
combines triggers. The input descriptor advertises no output/feature report;
rumble is **not implemented** and unsupported requests are rejected.

## Isolated conformance, run only with explicit authorization

```sh
python3 scripts/check-wine-input.py \
  --wine /absolute/path/to/the/selected/wine-loader \
  --arch arm64 \
  --prefix /absolute/path/to/a/NEW/disposable-prefix \
  --log-dir /absolute/path/to/test-results
```

The harness refuses an existing prefix, initializes only the new one, explicitly
installs the driver, and checks empty baseline → create → neutral → all button
bits/D-pad combinations/trigger and axis extremes → release → detach. It removes
the owned root device afterwards and retains logs/prefix for inspection.
`--frames-only` writes fixture bytes without any Wine execution or installation.

`consumer.exe` receives expected report/destroy frames through stdin. It reads
the **stock** XInput DLL and enumerates the **stock** DirectInput8 interface.
Every comparison is labelled `transport_only:true`, `steam_input_verified:false`.
It requires an otherwise-empty single-pad environment and identifies the owned
DirectInput device by its full `DIPROP_GUIDANDPATH`; additional controllers make this fixture fail rather than
accepting an unrelated first slot. It compares actual XInput data, including
Guide only when ordinal100 `XInputGetStateEx` is available. It currently proves
DirectInput enumeration, not DirectInput button/axis data.

Local adapter tests cover16 individual button bits,16 D-pad states, trigger
extremes, axis extremes, padding and descriptor length. Successful compilation
and these tests do not prove Wine installation or runtime delivery. Pending:
automatic ensure-mode runtime verification, multiple pads in reverse creation order,
stable Steam-slot/XInput-slot association, selective physical-device duplicate
filtering, genuine producer hookup and real physical-controller input.
