# Real Steam Input diagnostic through NotProton

This is independent of the normal Steamworks SDK preflight. It does not replace
any game DLL, implement a Steam interface, or create a controller.

Build without starting Wine, Steam or a game:

```sh
python3 scripts/build-input-probe.py
```

Outputs are `target/notproton/input-probe.exe` (x64) and
`target/notproton/input-probe32.exe` (x86). Both compile with `-Werror` and link
only the listed Kernel32 imports. `sdk-client.cpp` supplies the existing genuine
Steam identity verification, unchanged.

The runtime owner launches the probe through the same NotProton `steam.exe`
bootstrap as a game, with its real AppID, authenticated account, owned runtime,
private IPC routing and controller service kept alive. Use the same runner
environment as game launch. Do not use `runinprefix`: a new prefix requires the
client bootstrap to set up the genuine Windows client registry.

| Environment | Meaning |
| --- | --- |
| `SKINOS_EXPECTED_STEAMID` | Required identity to compare against the real bridge |
| `SKINOS_WINDOWS_CLIENT` | Optional DLL path; defaults to the architecture-correct Steam client in the Wine prefix |
| `SKINOS_INPUT_DURATION_MS` | Scan window, default 7000; accepted range 100–60000 |
| `SKINOS_INPUT_INTERVAL_MS` | Sampling interval, default 100; accepted range 10–1000 |
| `SKINOS_INPUT_ACTION_MANIFEST` | Optional absolute Windows path to a real action manifest |
| `SKINOS_INPUT_ACTION_SET` | Optional named action set to activate during this diagnostic |
| `SKINOS_INPUT_DIGITAL_ACTION` | Optional digital action to observe |
| `SKINOS_INPUT_ANALOG_ACTION` | Optional analog action to observe |

The caller also enforces an external deadline (15 seconds for the default scan):
an IPC call itself may block before the in-process scan deadline can be checked.
The runtime owns cleanup of its exact probe/prefix if that happens.

JSON lines on stdout:

- `steam_input_probe_ready`: genuine identity checked, SteamInput006 initialized;
  this says nothing yet about attached devices.
- `steam_input_snapshot`: genuine controller handles, types, active action sets,
  binding revisions, remote-play session IDs, session opt-in flags, and XInput
  slots. A routed XInput slot requires a currently enumerated Steam Input handle,
  a successful XInput read, and matching forward/reverse slot mapping.
- `steam_input_probe_summary`: scan completion, maximum controller count, real
  changes to correlated XInput or active Steam Input actions. Zero devices never
  produces `steam_input_activity_verified: true`.
- `steam_input_probe_error`: a failed identity/interface/initialization check.

`physical_device_verified` remains false: this program cannot establish that a
device is physical rather than Remote Play or a legitimate virtual device.
The runtime/device inventory and a user-observed button test supply that separate
evidence. Exit 0 means the diagnostic finished, not that controller support passed
a hardware test. The game does not need to use both action API and XInput; the two
paths are reported separately.

Analog values use `x_f32_bits`/`y_f32_bits` to preserve the exact returned IEEE-754
float without adding a Windows C runtime or changing values in the diagnostic.

ABI declarations follow Valve's pinned [SteamInput006 header](https://github.com/ValveSoftware/Proton/blob/164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca/lsteamclient/steamworks_sdk_153a/isteaminput.h)
and [SteamClient017 header](https://github.com/ValveSoftware/Proton/blob/164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca/lsteamclient/steamworks_sdk_142/isteamclient.h).
The old SDK preflight sources and behavior remain unchanged.
