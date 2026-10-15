# SkinOS for macOS

Native SwiftUI/AppKit frontend, Apple Silicon and macOS 15+. The app launches the bundled Rust core
and exchanges newline-delimited JSON over child-process pipes. No Steam URI is
used to launch games; the core owns authentication, downloads and game runtime.

Build from the repository root:

```sh
cargo build -p skinos-core -p skinos-runtime-host -p skinos-sdk-probe
bash scripts/build-app.sh
open dist/SkinOS.app
```

For UI development, build with `swift build --package-path apps/macos` and set
`SKINOS_CORE_PATH` to the actual `skinos-core` executable. The UI displays only
library records returned by the core. It contains no sample games or simulated
download progress.

The packaging script requires all three Rust executables (`skinos-core`,
`skinos-runtime-host`, and `skinos-sdk-probe`) and includes `libskinos_trace.dylib`
next to them in `Contents/MacOS`. It queries the helper
path with `skinos-runtime-host --print-trace-path`, signs the nested binaries,
verifies the bundle signature, and confirms that the packaged host resolves the
packaged library. The resulting app does not need the Cargo `target` directory.
The runtime's native launch and IPC-routing code is compiled into that same
`libskinos_trace.dylib`; there is no separate SkinOs IPC dylib to package.
The runtime host reads the SkinOs-owned runtime cache from
`~/Library/Application Support/SkinOs/runtime-source/current.json`. Its manifest
must identify the audited version and a directory inside that private source
root. On a fresh installation, the host prepares this cache from the pinned,
generic bundled runtime and verifies every file against its compiled manifest.
This payload contains no accounts, credentials, installed games or saved games.
The host does not fall back to the original Steam installation. Developer tools
are needed to build SkinOS, but are not needed to run the distributed app.

For release packaging, build all three Rust executables with `--release`, then use
`SKINOS_CONFIGURATION=release bash scripts/build-app.sh`. Custom build locations
can be passed through `SKINOS_CORE_PATH`, `SKINOS_RUNTIME_HOST_PATH`,
`SKINOS_SDK_PROBE_PATH`, and `SKINOS_TRACE_PATH`; `SKINOS_OUTPUT` selects the output app path. The default
signature is local ad-hoc signing, not Developer ID distribution or notarization.
See [DMG distribution](../../docs/implementation/dmg-distribution.md) for the
separate signed, notarized packaging and extracted-artifact validation steps.

The frontend supports QR/password/Steam Guard sign-in, library search, platform,
installation and playtime filters, favorites and sorting. Game details include
system requirements, store links, launch/stop controls and a selectable launch
log with refresh and explicit copy. Store and community profiles use an
account-isolated, nonpersistent WebKit session; friends and chat use the core's
Steam connection, with invisible presence on sign-in.

The Downloads screen follows the core's authoritative account-scoped queue.
Choose 1–4 concurrent jobs (default 2), pause/resume or cancel jobs, and retry
failed/cancelled entries without creating a new queue entry. Waiting downloads
can be reordered with arrows or the drag handle; running downloads are not
preempted. The game detail card exposes the same pause/resume/cancel controls.
Queue order, concurrency and downloaded byte counters are persisted by the core;
after reconnect/restart, unfinished entries are restored paused and can be
continued explicitly. Downloaded staging data is retained for verification and
reuse when continuing or retrying.

Settings → Downloads includes a searchable catalog of Steam download regions
and automatic selection. The saved choice applies when a job starts or resumes;
active transfers continue with their existing servers. The downloader prefers
eligible servers from the selected region and keeps Valve-provided fallbacks.

Network transfers show downloaded/total bytes, percentage, current speed and
estimated time. Verification/install phases show processed disk bytes and
file/chunk counters; stopped/waiting jobs do not show a running speed or ETA.
The UI rejects snapshots from another account or older queue revisions and does
not interpret an enqueue acknowledgement as a completed installation.

Account tokens are owned by the core; passwords and Guard codes are cleared
from input fields after sending and never logged. The UI never reads Steam
account/config files. Run offline protocol/model tests with
`swift test --package-path apps/macos`; these tests do not launch games, perform
network downloads or send chat messages.

Primary API references: [SwiftUI](https://developer.apple.com/documentation/swiftui),
[Foundation Process](https://developer.apple.com/documentation/foundation/process),
[Core Image QR generator](https://developer.apple.com/documentation/coreimage/cifilter/qrCodeGenerator()).
