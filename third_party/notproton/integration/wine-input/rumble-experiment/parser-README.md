# Actual Wine parser validation

Run `python3 scripts/test-wine-rumble-descriptor.py` from the repository root.
The run writes only `target/wine-rumble-descriptor-parser-test` by default.
It never launches Wine, installs a driver, uses Steam, or accesses controllers.

The runner checks the cached Wine archive SHA-256, extracts its headers and
**unchanged** `dlls/hidparse.sys/main.c` and `dlls/hid/hidp.c`, and compiles them
as a native macOS executable. It also extracts the unchanged
`check_waveform_caps` function from `dlls/xinput1_3/main.c`. The source pin is
[`290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c`](https://github.com/ValveSoftware/wine/tree/290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c).
Debug output and pool allocation use host shims. `HidD_GetFeature` is explicitly
a fixture I/O boundary that copies the actual `haptics.h` array; it is not live
device evidence. Unused parser exports, including scaled-value arithmetic, are
removed by the linker rather than emulated.

Verified by the real parser on 2026-10-14:

- One Generic Desktop / Gamepad top-level collection.
- Collection report lengths: input **15**, output **5**, feature **17** bytes,
  including the report-ID/prefix byte.
- Report ID **0** is input-only. Report ID **1** is output/feature-only.
- The complete internal input caps and public input button/value caps are
  byte-identical to the production canonical descriptor.
- The canonical input bytes precede `PUSH`; `POP` precedes the same app
  collection's closing tag.
- Stock XInput's actual waveform selector recognizes rumble collection **1**
  and buzz collection **4**, using the actual feature response array.
- Real `HidP_SetUsageValue` gives exact `[1, leftLE16, rightLE16]` output bytes
  for all **25** combinations of `0`, `1`, `0x1234`, `0x8000`, `0xffff`.
- ASan and UBSan pass with fail-fast enabled. Alignment instrumentation is
  excluded because upstream intentionally reads unaligned HID short items.

## Input offsets

Offsets here are in the **15-byte HidP report**, including byte 0's ID/prefix.
The transport's canonical payload remains 14 bytes; subtract 1 for its offsets.

| Field | HidP byte | Bit | Width |
|---|---:|---:|---:|
| Buttons usages 1–10 | 1 | 0 | 10 bits |
| Vendor Guide | 2 | 2 | 1 bit |
| Padding | 2 | 3 | 5 bits |
| Hat | 3 | 0 | 8 bits |
| Left trigger / Z | 4 | 0 | 8 bits |
| Right trigger / Rz | 5 | 0 | 8 bits |
| LX / X | 6 | 0 | 16 bits |
| LY / Y | 8 | 0 | 16 bits |
| RX / Rx | 10 | 0 | 16 bits |
| RY / Ry | 12 | 0 | 16 bits |
| Padding | 14 | 0 | 8 bits |

## Feature and output offsets

Both report types use byte 0 = `1`.

| Feature field | Byte | Width | Actual value |
|---|---:|---:|---:|
| Rumble waveform | 1 | 16 bits | `0x1005` |
| Rumble duration | 3 | 16 bits | 0 |
| Rumble cutoff | 5 | 32 bits | 1000 ms |
| Buzz waveform | 9 | 16 bits | `0x1004` |
| Buzz duration | 11 | 16 bits | 0 |
| Buzz cutoff | 13 | 32 bits | 1000 ms |

Output left intensity occupies bytes 1–2; right intensity occupies bytes 3–4.
Both are unsigned 16-bit little endian. The feature logical maximum must use
the 32-bit item `27 ff ff 00 00`: the pinned parser treats `26 ff ff` as -1.

`report.json` records source hashes, owned descriptor hashes, sanitizer mode,
process result, and exact parser output. A successful native test establishes
descriptor/parser compatibility only. Live `HidD_GetPreparsedData`, the
driver's GET_FEATURE/output paths, XInput delivery and cleanup remain separate
runtime checks owned by the parent test runner.
