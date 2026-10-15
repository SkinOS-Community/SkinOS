# SkinOS

A native Steam client for macOS, built with **Rust** and **SwiftUI/AppKit**.
SkinOS connects to Steam for your account, library, downloads and friends, and
uses its own game runtime process. The original Steam application is not used
as a game launcher.

[Download releases](https://github.com/SkinOS-Community/SkinOS/releases) ·
[Report a bug](https://github.com/SkinOS-Community/SkinOS/issues/new/choose) ·
[Discussions](https://github.com/SkinOS-Community/SkinOS/discussions) ·
[Contributing](CONTRIBUTING.md)

## What works

- Steam sign-in and saved accounts, library search, filters and favorites.
- Parallel game downloads and updates, pause/resume, cancellation and queue
  ordering, with transfer speed, progress and estimated time remaining.
- Explicit macOS/Windows selection before downloading, with macOS preselected
  when available. The chosen version is retained across pauses and updates.
- Searchable Steam download regions with preferred servers and fallbacks.
- Automatic cleanup of obsolete download staging after a verified completed
  installation, preserving files needed by paused or unfinished transfers.
- Native macOS games and Windows games through pinned NotProton integration
  with a separately installed CrossOver or CrossOver Preview.
- Native launch plans use Steam option IDs and installed bundle/binary/script
  metadata. Auto-Cloud save paths are resolved without per-game AppID rules;
  unsupported paths and launch formats produce specific diagnostics.
- Per-game launch arguments and compatibility settings, launch/stop controls,
  installation removal and redacted launch diagnostics.
- Steam store and profile pages, friends and text chat, with invisible presence
  on sign-in and native notifications.
- Signed application updates from GitHub Releases through Sparkle. Installation
  requires confirmation and waits for active games and downloads to stop.

This is an active community project. It does not claim complete Steam feature
parity or compatibility with every game. Steam Input and the Wine controller
bridge are experimental; the current implementation targets one controller.
See the implementation notes in [docs](docs/) for tested behavior and remaining
work. Contributions and reproducible compatibility reports are welcome.

## Install

Download the Apple Silicon DMG from
[Releases](https://github.com/SkinOS-Community/SkinOS/releases), drag SkinOS into
Applications, and sign in with your own Steam account.

- **Apple Silicon, macOS 15 or later.**
- Windows games require a compatible, separately installed **CrossOver** or
  **CrossOver Preview**. NotProton's FEX mode requires macOS 26 or later.
- Games require the appropriate account entitlements; Steam authorizes content
  downloads. SkinOS does not include games or another person's account data.

The public application fetches pinned external runtime components from their
upstream sources when needed. Archives and individual files are verified before
use. Valve's proprietary libraries are separate dependencies, not code licensed
under this repository's GPL license. CrossOver is not bundled.

Versions before the first updater-enabled release need one manual upgrade.
Later releases are checked through the signed GitHub appcast; automatic checks
are optional and installation is never forced during a game or transfer.

## Development

Start with the [development guide](docs/development.md). Source checks require a
Mac with Xcode Command Line Tools, Rust 1.88 or newer, and Swift 6 or newer:

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
swift test --package-path apps/macos
```

Full application builds additionally need the pinned NotProton source and
runtime inputs. The bootstrap/build scripts fetch or validate these inputs;
compiled third-party archives, caches and credentials are excluded from Git.
See [distribution](docs/implementation/dmg-distribution.md) for signing and
notarization, and the updater scripts for signed release feeds.

## Community

- Use [issues](https://github.com/SkinOS-Community/SkinOS/issues) for reproducible
  bugs and actionable proposals, and
  [Discussions](https://github.com/SkinOS-Community/SkinOS/discussions) for help,
  compatibility reports and larger design ideas.
- Read [CONTRIBUTING](CONTRIBUTING.md) before opening a pull request.
- [GOVERNANCE](GOVERNANCE.md) explains review, maintainership and release policy.
- Follow the [Code of Conduct](CODE_OF_CONDUCT.md).
- Report vulnerabilities privately using [SECURITY](SECURITY.md). Never attach
  passwords, session tokens, cookies, account databases or unredacted logs.

Community-driven means public proposals, reviewable changes and reproducible
checks. Maintainers review changes and sign releases; pull requests from forks
never receive release-signing credentials.

## License and attribution

SkinOS-authored code is **GPL-3.0-only**. See [LICENSE](LICENSE),
[NOTICE](NOTICE) and [third-party notices](THIRD_PARTY.md). Third-party components keep their own licenses, including
NotProton, Wine, steam-cm-protocol and Sparkle. Downloaded proprietary runtime
components are not relicensed by this project.

Steam and Steamworks are trademarks of Valve Corporation. SkinOS is an
independent project and is not affiliated with or endorsed by Valve.
