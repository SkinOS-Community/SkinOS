# Third-party components

SkinOS-authored code is licensed under GPL-3.0-only. Third-party components retain their own licenses, notices and copyrights; the root license does not replace them.

## Rust dependencies

`Cargo.lock` records the resolved crate versions. Their declared license expressions and repository URLs come from Cargo package metadata. Before packaging a release, collect the actual license and notice files supplied with those packages:

```sh
python3 scripts/collect-third-party-licenses.py \
  --target aarch64-apple-darwin \
  --output dist/SkinOS.app/Contents/Resources/licenses/Rust
```

The generated `index.json` lists package name/version, declared license expression, upstream repository, license-file paths and SHA-256 hashes. Texts are copied without rewriting copyright statements. The collector uses the locked target dependency graph, including build/test dependencies, so its inventory can be broader than the code linked into the executable. It fails if a selected registry package lacks original license text; it does not manufacture a replacement from an SPDX identifier.

The generated directory also contains available Rust standard-library license/copyright notices from the active toolchain, plus component notices when `rust-src` is installed. Record and use the same Rust toolchain for compilation and notice collection. Generated notices belong in release resources; registry caches and local build paths do not belong in the public source repository.

The vendored `steam-cm-protocol` package is maintained separately under its included license. Preserve its upstream attribution and the local modification history when redistributing source or binaries.

## Native UI, compatibility and input

- **Sparkle** provides signed application updates. Preserve the license and notices from the exact framework release used by the app.
- **NotProton** is pinned in `third_party/notproton/UPSTREAM.json`; its source, notices and component licenses remain separate from SkinOS-authored integration code. The public bundle includes the compiled launcher/setup integration and the corresponding notices.
- **Wine-derived input integration** retains the Wine license and source notices under `third_party/notproton/integration/wine-input`. Preserve those notices in packaged input components.
- Apple system frameworks are provided by macOS. CrossOver/Preview is separately installed and licensed by its user.

## Separately downloaded dependencies

The public repository and public application release do not redistribute Valve runtime binaries, proprietary game files, NotProton bridge binaries or fonts. When needed, the app retrieves pinned external dependencies from their original publishers and verifies archive/member checksums. Those downloads remain subject to their respective licenses and terms; a hash pin is an integrity check, not a redistribution license.

NotProton source and local build inputs can be obtained using `scripts/bootstrap-notproton.py`. Keep downloaded files in ignored local directories. Do not add them to the source publication or copy them into a public release to bypass the public packaging mode.
