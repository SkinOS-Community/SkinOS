# Separate DirectInput state consumer

Build with `python3 scripts/build-wine-input-di.py`. Output is only
`target/wine-input-di-experiment/{arm64,x64,x86}/consumer-dinput.exe`; this command
does not replace the live `target/wine-input` consumer, driver, feeder or manifest.
All three compiled with `-Werror`. Root-owned DI2 testing on CrossOver Preview
confirmed the button/D-pad/trigger/axis/combined/release comparisons, including
actual Y extremes. A later reconnect case failed because the new DI consumer
acquired after the held report; the complete lifecycle run remains unfinished.

After authorization, select this executable in the ordinary isolated harness
with `--consumer-path target/wine-input-di-experiment/arm64/consumer-dinput.exe
--consumer-arch arm64`. `--arch` continues to select the native driver/feeder.
For a32-bit consumer, select the x86 experiment path plus `--consumer-arch x86`.
The harness verifies the consumer manifest hash and PE architecture before
creating the new prefix. Its separate lifecycle phase closes a held-state
feeder by EOF without Destroy, requires disappearance, then reopens cleanly.

The stdin expectation protocol and JSONL event names match the basic consumer.
The real public DirectInput8 interface enumerates devices, reads
`DIPROP_GUIDANDPATH`, verifies our complete IG interface path/slot/generation,
opens that instance, configures an explicit36-byte data format, acquires it,
polls, and calls `GetDeviceState`. Every axis gets range0..65535, deadzone0,
saturation10000; these are independently verified through `GetProperty` before
any state comparison. It refuses an environment containing another gamepad.

The first live target read confirmed API-reset neutralY/RY32768. This matches
Wine `dinput/device.c:reset_object_value`, which rounds the configured range
centre32767.5 upward after `SetProperty(DIPROP_RANGE)`. Only when the original
signed Y/RY is0 does the oracle accept either32767 (processed report) or32768
(API-centred state). JSON records the exact observed values and a separate
`dinput_center_rounding_mask`. No edge/nonzero input or other field gets tolerance.

Expected values come directly from the original20-byte Xbox semantic report,
not from `adapter.h` output. For the legacy IG interface:

| Original value | DirectInput value |
|---|---|
| X/RX -32768,0,32767 | 0,32768,65535 |
| Y/RY -32768,0,32767 | 65535,32767,0 |
| LT255/RT0 | Z65408 |
| LT0/RT255 | Z128 |
| LT0/RT0 or LT255/RT255 | Z32768 |
| D-pad released | POV0xffffffff |
| Up/right/down/left | POV0/9000/18000/27000 |
| A/B/X/Y/LB/RB/Back/Start/LStick/RStick | buttons0–9, pressed byte0x80 |

Opposing D-pad directions cancel per axis. Reserved bytes/bit11 never become
input. This legacy interface combines triggers and omits Guide; independent
triggers and Guide remain checked through stock XInput. Neutral/release requires
all10 button bytes0, centre axes as above and POV0xffffffff; mere enumeration or
a successful API return never counts as state delivery.

The source basis is [Wine winexinput translation](https://github.com/ValveSoftware/wine/blob/290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c/dlls/winexinput.sys/main.c)
and [DirectInput HID scaling](https://github.com/ValveSoftware/wine/blob/290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c/dlls/dinput/joystick_hid.c).
Public ABI declarations are the bounded subset from the same revision's
`include/dinput.h`, with struct size/vtable offset compile assertions.

**Source prediction superseded by target evidence:** the pinned Valve Wine
translation negates an
unsigned raw Y before its sign extension. [HidP_GetUsageValue](https://github.com/ValveSoftware/wine/blob/290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c/dlls/hid/hidp.c)
zero-fills its32-bit output then copies the raw field, so a signed16 neutralY0
becomes unsigned0xffffffff and then signed-65537, outside the declared range.
That source predicted an incorrect DI Y0 for that path. The installed CrossOver
Preview did **not** reproduce it: DI2 passed every tested Y extreme with the
existing descriptor. No descriptor or adapter correction is justified by that
earlier source-only prediction.

The reconnect/EOF fixture now waits for `reopen_neutral_ready` before sending
held state. Wine's DI Acquire starts a new overlapped ReadFile after range
configuration reset. The stock winexinput IG child rejects GET_INPUT_REPORT;
therefore the bus's latest-state GET_INPUT_REPORT implementation is not a
reliable late-acquisition snapshot path for that consumer. The handshake tests
EOF cleanup with an acquired reader. It does not prove a game's late acquisition
while a button was already held; that remains a separate product scenario.
