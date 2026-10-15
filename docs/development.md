# Development from source

SkinOS has a native SwiftUI/AppKit frontend and Rust services. Distributed builds currently target Apple Silicon and macOS 15 or later. Windows game execution additionally requires a separately installed CrossOver or CrossOver Preview; the FEX route requires macOS 26 or later.

## Source checks

Use a Mac with Xcode Command Line Tools, Swift 6 or later, Python 3.9 or later and current stable Rust. The core declares Rust 1.88 as its minimum; the workspace-level declaration alone is not the minimum for the whole application. CI uses current stable Rust and the `macos-15` Apple Silicon runner.

From the checkout root:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
swift test --package-path apps/macos
python3 scripts/test-bootstrap-notproton.py
```

These checks compile the native adapter and exercise local fixtures, model behavior and loopback transports. They do not require Steam sign-in, installed games, CrossOver, Apple signing keys or Valve runtime binaries. Dependencies may be fetched from their package registries. The pinned runtime roundtrip test requires an explicit local fixture and otherwise skips; the signed bridge fixture test is ignored by default. Their coverage is not implied by a green public CI run.

The repository must retain the vendored protocol `.proto` sources, `third_party/notproton/UPSTREAM.json`, the owned Wine input descriptor, public downloader fixtures and the runtime hash manifest. These are build/test inputs, not a developer's runtime cache.

For a faster edit/build cycle:

```sh
cargo build --locked -p skinos-core -p skinos-runtime-host -p skinos-sdk-probe
swift build --package-path apps/macos
```

The UI accepts `SKINOS_CORE_PATH` pointing to the built core executable. Building these executables does not produce a fully provisioned game-launch bundle.

## Isolate development data

Set `SKINOS_DATA_DIR` to a dedicated local directory before running development executables with an account. It redirects SkinOS state, including its account registry and runtime cache. The game-library path is a separate setting: this variable alone does not redirect or isolate your Steam games. Choose a separate library before testing installation, update or removal behavior.

Debug builds default to file-based refresh-token storage with restricted filesystem permissions. Release builds default to Keychain. `SKINOS_CREDENTIAL_STORE=file` or `keychain` explicitly selects a backend. Refresh tokens are credentials even though they are not passwords. Do not commit, upload or attach this directory to a bug report.

Avoid running multiple clients against the same active game installation or account state while testing writes. Never automate purchases, chat messages or destructive tests using someone else's account.

## Full app packaging

`scripts/build-app.sh` assembles a complete app. Its default mode is `SKINOS_DISTRIBUTION=public`: the bundle contains the owned client and launcher, and retrieves the exact external game-runtime and compatibility dependencies directly from their publishers when first needed. These dependencies are separately licensed; public repository/release assets do not include the Valve runtime cohort, NotProton bridge binaries or fonts.

Bootstrap the pinned NotProton source and local build inputs, then build:

```sh
python3 scripts/bootstrap-notproton.py
cargo build --locked -p skinos-core -p skinos-runtime-host -p skinos-sdk-probe
bash scripts/build-app.sh
```

The bootstrap downloads an exact source commit and pinned release/package archives over HTTPS, verifies archive sizes/SHA-256 and selected member hashes, and writes only ignored local inputs. It refuses to overwrite edited upstream source. `--source-only` retrieves the upstream source without bridge/font build inputs. These files are build dependencies, not material to add to a public source archive.

First use of a public app requires access to the publisher download hosts. Verified dependencies are cached in SkinOS's data directory and reused; their versions do not silently follow upstream's latest release. An unavailable or changed pinned download fails verification instead of loading an arbitrary replacement.

The legacy `SKINOS_DISTRIBUTION=bundled` mode is only for local private testing with separately supplied inputs. In that mode, `SKINOS_RUNTIME_SOURCE` selects a dedicated source directory for the pinned runtime payload builder. Never publish that bundle or copy an entire Steam installation, account directory or a release maintainer's data into the checkout to make a build pass.

Local app assembly uses ad-hoc signing. Official distribution additionally needs the separate signing, notarization and packaged-artifact checks described in [DMG distribution](implementation/dmg-distribution.md). Those operations are not part of public pull-request CI.

## Manual coverage

Game launch, Steam network behavior and physical controllers need separate tests. Record the source revision, app version, game AppID, OS, runtime/CrossOver version and what you actually observed. Keep credentials and private account data out of evidence. Steam Input is experimental and has limited physical-controller coverage; offline tests do not establish hardware support.

GitHub runner and workflow configuration follows the official [runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) and [secure-use guidance](https://docs.github.com/en/actions/reference/security/secure-use). Action revisions are pinned to upstream commits; dependency updates should be reviewed like other code changes.
