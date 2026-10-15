# Public source and release boundary

Audit date: 2026-10-15. This is an inventory and engineering recommendation,
not a declaration that all existing build artifacts have redistribution rights.
The audit did not open account stores, credential files, the Keychain, or chat
history. No files were uploaded.

## Decision

Publish a deliberately selected source tree. Do not upload the existing working
directory, its build caches, or the existing DMGs wholesale. A source release
and a self-contained binary release have different third-party dependencies.

For SkinOS-authored code, GPL-3.0-only is the recommended project license:
`scripts/build-notproton.py` compiles NotProton Swift files into the setup helper
and adapts its launch script. NotProton's principal license is GPLv3. Retain
the separate MIT, BSD, LGPL, and SDK notices for their respective components;
the root license must not purport to relicense them. The project owner must
choose the license before the first public source commit.

## Proposed source allowlist

The following paths are suitable candidates after the concrete checks below.
This is an allowlist for an export/staging directory, not `git add .`.

| Path | Treatment |
| --- | --- |
| `Cargo.toml`, `Cargo.lock`, `.gitignore` | Include; set explicit project license and expand ignores before publication. |
| `README.md`, new community and license documents, `.github/` | Include after checking links, actual support claims, workflow permissions, and secret-free configuration. |
| `apps/macos/Package.swift`, `Package.resolved`, `Sources/`, `Tests/`, `Tools/`, `README.md` | Include. No app bundle or Swift build cache. |
| `crates/` | Include source, manifests, resource metadata, and test fixtures; omit Cargo-generated `.cargo-ok`. |
| `scripts/` | Include authored `.py`, `.sh`, `.c` and tests; omit `__pycache__` and outputs. Publishing build recipes is different from publishing their binary inputs. |
| `docs/` | Include after replacing the three author-specific paths listed below and reviewing historical verification claims. |
| `third_party/notproton/UPSTREAM.json` | Include as an exact upstream revision pin. |
| `third_party/notproton/integration/` | Include authored integration and Wine-derived source with both `LICENSE.Wine` files and existing notices; sanitize its README path. |
| `third_party/notproton/bridge/manifest.json` | Metadata-only exception may be included if the exporter explicitly selects it. Current provenance is public upstream release URLs, IDs, member names, and hashes. Do not include the sibling gzip files. |

The runtime pin (`crates/runtime-host/resources/runtime-payload-pin.json`) is
metadata: relative generic file names, hashes, sizes, and fixed link targets.
It contains no executable bytes or copied user configuration. Likewise,
`crates/downloader/tests/fixtures/dota2-public-appinfo.json` is the public depot
metadata fixture documented by its sibling README. The vendor's
`tests/fixtures/userstats_schema_410110.bin` is a 31,164-byte schema test fixture,
not an executable, and `achievements.rs` includes it at compile time. A blanket
ban on every `.bin` would accidentally break those upstream tests.

## Explicit exclusions

- `target/`, `dist/`, `outputs/`, all DMGs/ZIPs/app bundles, native delivery
  carriers and recipes generated for distribution, runtime payload archives,
  crash dumps, screenshots, recordings, captured reports, and local test state.
- `upstream/` (unrelated experimental checkout), `apps/macos/.build/`, nested
  Git metadata, `.DS_Store`, `__pycache__`, and package-manager cache markers.
- `third_party/notproton/upstream/` for the first export. Fetch the exact
  `UPSTREAM.json` commit as a separate dependency step. The local snapshot is
  349 files / approximately 117 MB, including fonts and twelve binary patches;
  it should not silently become SkinOS-authored code under the root license.
- `third_party/notproton/bridge/` except the explicitly selected metadata file.
  Its 31 gzip artifacts include Valve client DLLs and native bridges, as well
  as fonts. Presence in another project's release is not evidence of a general
  open-source license for every artifact.
- Any local credentials, cookies, refresh/access tokens, account databases,
  game manifests containing user state, saves, logs, private signing material,
  provisioning profiles, or local user configuration, wherever stored.

For GitHub Releases, exclusions apply independently of Git's ignore rules.
The old DMGs include the 3,813-file Valve runtime/input cohort: notably
`steamclient.dylib`, `libsteaminput.dylib`, controller resources, and supporting
libraries. No repository evidence establishes permission to redistribute that
whole Steam client cohort. Do not attach those existing DMGs automatically to
the new public repository. Establish component-specific rights or ship a
distribution that imports required proprietary components from a legitimate
local source; keep the application and updater's own signed artifacts separate.
This is a release-scoping blocker, not a reason to withhold authored source.

## License evidence

| Component | Evidence and consequence |
| --- | --- |
| SkinOS-authored source | No root `LICENSE` existed at audit start. Apply the chosen license prospectively to authored code and identify exceptions. |
| Vendored `steam-cm-protocol` 0.4.1 | Its local `LICENSE` is MIT, with the original copyright notice; `Cargo.toml` declares MIT and upstream repository. Preserve that license and attribution. |
| NotProton principal source | Pinned upstream `LICENSE` is GPLv3; `NOTICE` explicitly distinguishes its differently licensed components. Distributing modified helper/source must retain notices, disclose modifications, and supply corresponding source/build scripts. |
| `lsteamclient` | Its local `LICENSE` is the Steamworks SDK License, not GPL. The pinned `fetch.sh` intentionally fetches Valve's SDK-derived baseline rather than vendoring it. Keep that separation. |
| NotProton `steam-shim` | `LICENSE.steam-shim` is BSD-3-Clause; upstream `NOTICE` separately identifies Steamworks, OpenVR, and Wine header terms. |
| Wine input integration | `driver.c` declares LGPL-2.1-or-later and original Wine attribution. Both integration trees include `LICENSE.Wine`; preserve source and notices. |
| Fonts, cJSON, JsonCpp, Sparkle | NotProton's `NOTICE` identifies their separate licenses. Font build scripts and font families have different licenses. A future binary release needs an exact component inventory and corresponding notices. |

Primary references, checked on the audit date:

- [Pinned NotProton license](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/LICENSE)
- [Pinned NotProton notices](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/NOTICE)
- [Pinned lsteamclient license](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/lsteamclient/LICENSE)
- [Pinned lsteamclient fetch boundary](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/lsteamclient/fetch.sh)
- [Steamworks SDK documentation](https://partner.steamgames.com/doc/sdk) identifies `redistributable_bin`; it does not grant a license to repackage the entire installed Steam client.
- [Steam Subscriber Agreement, sections 2.F–G](https://store.steampowered.com/subscriber_agreement/) reserves rights and limits copying/distribution absent permission.

## Privacy scan findings

Content scans covered authored Rust/Swift/C/C++/Python/shell files, source JSON
and Markdown, tests, and the selected vendored protocol code. Known secret
formats and long literal password/token assignments produced no live-secret
finding. This is a bounded source scan, not a guarantee about excluded folders.

Three documents contain real author home-directory paths and should use
`$HOME` or a documented argument instead:

- `docs/research/goldsrc-compat.md`, lines 11–12.
- `docs/research/goldsrc-crash.md`, lines 39–42 and 55.
- `third_party/notproton/integration/wine-input/rumble-experiment/README.md`, line 115.

SteamID matches inspected in source/tests were synthetic fixtures. The JWT
literal in `crates/client-core/src/diagnostics.rs` is a deliberately invalid
redaction test with a literal `signature`, not an authentication credential.
`/Users/fixture` in cleanup tests is synthetic. Do not replace these solely
because a pattern scanner matches them.

## Build and publication gates

1. Export only the selected paths into a fresh staging tree. Scan that exact
   tree again and inspect `git diff --cached --stat` and file inventory before
   the first push. No existing local build artifact is implicitly approved.
2. Fetch NotProton from its exact revision into its ignored dependency path.
   Some Python integration tests read its source directly. The ignored compat test
   `signed_native_delivery_fixture_restores_all_three_pinned_bridges_offline`
   additionally expects local bridge artifacts; CI must explicitly distinguish optional integration input
   from ordinary source-only unit tests.
3. Document that the current full `scripts/build-app.sh` still requires local
   proprietary runtime inputs, bridge artifacts, and CrossOver import sources.
   A source-only checkout does not yet produce the same self-contained DMG.
4. Generate a transitive dependency license inventory from the lockfile before
   a binary release. An attempted offline `cargo metadata` was blocked by the
   uncached package `arbitrary 1.5.0`; no dependencies were fetched or changed
   during this audit, so that broader inventory is not claimed complete.
5. Publish updater metadata only for reviewed, signed release artifacts. Keep
   update-signing private keys and Apple signing/notarization credentials out
   of Git, pull-request jobs, artifacts, logs, and public workflow outputs.
   Fork PRs must not execute with release credentials.

The source-only publication can proceed after license selection, the three
path edits, dependency bootstrap documentation, and exact staged-tree review.
The existing all-in-one binary release needs the separate component-rights
and reproducibility work described above.
