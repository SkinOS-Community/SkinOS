# Steam Input through the owned runtime and NotProton

## Established from primary sources

The pinned NotProton bridge carries Valve's SteamInput006/007 bindings.
`GetConnectedControllers` and slot-mapping calls forward to the native Steam
interface. The manifest setter converts a Windows path to its Unix equivalent
before forwarding. These are genuine API calls, not replacement input data.
Sources: [NotProton input wrappers](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/lsteamclient/unix_steam_input_manual.cpp),
[pinned Valve generated SteamInput006 bridge](https://github.com/ValveSoftware/Proton/blob/164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca/lsteamclient/cppISteamInput_SteamInput006.cpp).

Steam Input action API and legacy gamepad emulation are separate output paths.
Valve documents a driver-backed virtual gamepad on macOS/Linux, versus API hooks
in the Windows overlay. A successful raw XInput read alone therefore does not
prove Steam Input routing. The diagnostic correlates both public mapping APIs
with the currently enumerated Steam Input handle and then observes actual state
changes. [Valve gamepad emulation documentation](https://partner.steamgames.com/doc/features/steam_controller/steam_input_gamepad_emulation_bestpractices).

SteamInput006 requires `RunFrame` before connected devices can be enumerated.
`Init` success is not evidence of connected hardware. `GetDeviceBindingRevision`
can distinguish a loaded binding from a controller whose mapping is not ready.
The declarations used by the probe are pinned to
[Valve SDK 1.53a](https://github.com/ValveSoftware/Proton/blob/164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca/lsteamclient/steamworks_sdk_153a/isteaminput.h).

The NotProton runner consumes `SDL_GAMECONTROLLER_IGNORE_DEVICES` and its exception
list to suppress duplicate Wine raw-HID reads for Steam Input-owned devices.
It records and changes only its owned `Hidraw` settings. Switch VID/PIDs have
additional upstream handling. This logic hides duplicate input; it does not
create a Steam virtual controller.
[Pinned runner](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/dylib/feats/compat_run.sh).

`SteamVirtualGamepadInfo` is metadata associated with an existing virtual gamepad,
not an input event source. Current upstream SDL reads identities by slot from
that file. Valve Wine's SDL bus explicitly avoids one such metadata override to
retain its virtual-controller slot naming. This is source evidence for those
upstreams, not proof of the exact shipped CrossOver implementation.
[SDL implementation](https://github.com/libsdl-org/SDL/blob/main/src/joystick/SDL_steam_virtual_gamepad.c),
[Valve Wine SDL bus](https://github.com/ValveSoftware/wine/blob/bleeding-edge/dlls/winebus.sys/bus_sdl.c).

SDL's macOS IOKit backend recognizes the Steam virtual-pad slot from a
`GamePad-N` product name on the Xbox 360 VID/PID and checks the virtual-device
property to avoid misidentifying a real Xbox controller. This gives the host
diagnostic a concrete inventory to inspect after its genuine driver publishes
devices; writing the metadata file alone cannot produce that inventory.
[SDL macOS backend](https://github.com/libsdl-org/SDL/blob/main/src/joystick/darwin/SDL_iokitjoystick.c).

## Game-specific bindings

An owned action manifest can refer to per-controller configuration files using
relative paths. `SetInputActionManifestFilePath` supplies a local override that
lasts for the Steam session. This permits testing owned action definitions and
configurations without opening the Steam interface. It does not, by itself,
establish the legacy XInput configuration or publish a virtual device.
[Valve action manifest documentation](https://partner.steamgames.com/doc/features/steam_controller/action_manifest_file).

The probe accepts optional manifest/action-set/digital/analog names and reports
only real active-action data. No default controller mapping has been invented or
reported as applied. Host-side configuration must be confirmed against the real
controller service before a settings screen claims it is active.

Read-only inventory of the owned runtime found genuine v3 `Standard Gamepad`
templates under `runtime-source/<audited-runtime-hash>/controller_base/templates`:
`controller_xboxone_gamepad_joystick.vdf` (SHA-256
`a69a66f77ffe18813efe4705f52f59eb450eebe2a58564cece731eb9da06f88a`),
`controller_generic_gamepad_joystick.vdf` (`57d41acf506a8a492852d465a4531d9068288538e4736dea92a5f80ed9e523fa`),
and `controller_apple_gamepad_joystick.vdf`. The Xbox template includes real
`xinput_button A` bindings and a Default preset with gamepad input groups. The
unprefixed `gamepad_joystick.vdf` is an older Steam Controller/trackpad template,
not the preferred Xbox fixture layout.

Independent static inspection of Valve-shipped `steamui/chunk~2dcc5aaf7.js`
(SHA-256 `f9606b111203c9140dcdd6fc016a0ee00e4c8406aeb786187c42873f08d071fb`)
identified this JavaScript endpoint flow:

```text
SetSelectedConfigForApp(appID, controllerIndex, configurationURL, false, selectionType)
StartEditingControllerConfigurationForAppIDAndControllerIndex(appID, controllerIndex)
SetEditingControllerConfigurationInputBinding(controllerIndex, base64Protobuf)
SaveEditingControllerConfiguration(controllerIndex, selectionType, false)
```

The selected URL comes from preview metadata or `ProgenitorURL`. Only
`template://empty.vdf` was found as a literal; actual gamepad template URLs come
from `QueryControllerConfigsForApp` and controller-config info messages, so their
resolver must still be verified. The native ABI and the meaning of the fourth
selection argument are not established by these JavaScript call sites. The
generated C# interface indices are known to differ from the loaded native
interface and must not be called blindly. This is ordinary static artifact
evidence, not runtime execution or REA Evidence.

## Implemented diagnostic

`third_party/notproton/integration/input-probe.cpp` connects through the same
genuine SteamClient017 bridge used by the existing identity probe, asks for
SteamInput006, then performs a bounded seven-second scan. Separate x86 and x64
binaries are built by `scripts/build-input-probe.py`. It leaves the normal SDK
preflight unchanged. See `third_party/notproton/integration/INPUT-PROBE.md` for the
environment and output contract.

The probe never opens a binding panel, changes haptics/LEDs, creates a controller,
or starts the official Steam app. An optional action-set override is restricted
to the diagnostic process/session. The owning runtime must pump its real engine
and impose an external timeout for an IPC stall.

## Remaining verification

- Runtime controller-service initialization and its dependencies: owned by the
  runtime-host implementation and its live diagnostics.
- Actual `SteamInput006` connection and a truthful zero-device report: requires
  the root-owned live authenticated test.
- Driver/virtual-gamepad delivery into the shipped CrossOver XInput path and
  game-specific bindings: requires real runtime evidence; enumeration or a raw
  generic gamepad is insufficient.
- Physical controller button/axis verification is deferred until the user has a
  controller. No physical controller result is claimed from offline builds.
