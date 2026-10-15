# DMG distribution

The distribution targets Apple Silicon and macOS 15+. CrossOver or CrossOver
Preview is installed separately for Windows games. NotProton is included; its
FEX mode needs macOS 26+, while the Rosetta mode supports macOS 15.

`scripts/build-app.sh` defaults to `SKINOS_DISTRIBUTION=public`. It packages
SkinOS, Sparkle, the NotProton integration and dependency metadata. Proprietary
runtime libraries, bridges and fonts are fetched directly from their pinned
upstream sources when needed, with archive and per-file SHA-256 validation.
Existing valid caches are reused. Accounts, credentials, games and saved games
are never distribution inputs. See [runtime bootstrap](public-runtime-bootstrap.md).

Before a full app build, run `python3 scripts/bootstrap-notproton.py` to fetch
the ignored build inputs. A private local `SKINOS_DISTRIBUTION=bundled` profile
remains for historical testing; do not publish its all-in-one artifacts as an
open-source distribution.

## Build and prepare

Use a fresh work directory for every distribution attempt. The staging copy is
separate from the development app so packaging does not interrupt its session.

```sh
MACOSX_DEPLOYMENT_TARGET=15.0 cargo build --release \
  -p skinos-core -p skinos-runtime-host -p skinos-sdk-probe
MACOSX_DEPLOYMENT_TARGET=15.0 SKINOS_CONFIGURATION=release \
  SKINOS_OUTPUT="$PWD/dist/release/SkinOS.app" bash scripts/build-app.sh
python3 scripts/package-dmg.py prepare --app dist/release/SkinOS.app \
  --work outputs/dmg-work --identity "$SKINOS_SIGN_IDENTITY"
```

For the private bundled profile only, preparation exposes native payload code as ordinary Developer ID
signed Mach-O files in `Contents/Resources/NativeRuntimeCode`. A bounded recipe
restores the upstream file layout from these signed files, with literals limited
to original Mach-O headers, code-signature metadata and verified zero padding.
The pinned steamclient also retains its 144-byte VLV container header at offsets
48–192, outside both executable slices; this exception is restricted to its
exact original file hash, fixed range and known header format.
Executable code and data must remain byte-identical in the signed carrier. The
restored files must also match their independent original SHA-256 pins. This is
needed because the audited runtime uses exact fingerprints and the x86 Wine
Unix bridge requires the original unsigned layout. Executable payload code is
not hidden in encrypted or opaque archives. Public DMGs contain neither these
carriers nor the compressed proprietary payload.

Sparkle 2.10.0 is copied with its nested helpers, licenses and framework links.
`sign-sparkle.py` checks the five expected native binaries and signs inside out
while preserving their original entitlements. Release builds embed the public
update key from `release/update-public-key.txt` and the GitHub appcast URL;
debug builds leave updating disabled. Private signing keys stay in Keychain.

The owned launch helpers that load the runtime use only
`allow-dyld-environment-variables` and `disable-library-validation`. Other owned
executables receive hardened runtime without these exceptions. Carrier files
are signed before their hashes are recorded; they must not be re-signed after
the recipes are generated.

## Notarize and package

Use a configured notarytool Keychain profile. No signing secrets belong in this
repository. Check that each submission reports `Accepted` before proceeding:
notarytool can return process exit code zero even when the submission is invalid.

```sh
xcrun notarytool submit outputs/dmg-work/SkinOS-notary.zip \
  --keychain-profile "$SKINOS_NOTARY_PROFILE" --wait --output-format json
# Continue only after status == Accepted.
xcrun stapler staple outputs/dmg-work/SkinOS.app
python3 scripts/package-dmg.py dmg --work outputs/dmg-work \
  --output outputs/SkinOS-0.1.3-AppleSilicon.dmg --identity "$SKINOS_SIGN_IDENTITY"
xcrun notarytool submit outputs/SkinOS-0.1.3-AppleSilicon.dmg \
  --keychain-profile "$SKINOS_NOTARY_PROFILE" --wait --output-format json
# Continue only after status == Accepted.
xcrun stapler staple outputs/SkinOS-0.1.3-AppleSilicon.dmg
```

## Verify the shipped artifact

```sh
python3 scripts/package-dmg.py verify --work outputs/dmg-work \
  --output outputs/SkinOS-0.1.3-AppleSilicon.dmg
python3 scripts/check-distribution-startup.py \
  --app outputs/dmg-work/extracted/SkinOS.app \
  --work outputs/dmg-work/extracted-clean-check
```

Verification mounts the exact DMG read-only, copies its app, checks all signatures
and stapled tickets, and runs Gatekeeper assessment on the DMG and extracted app.
It writes a SHA-256 file and a verification report. The separate startup check
uses a fresh data directory, no developer tools, no account and no game launch.
It verifies every restored runtime file, cache reuse, the bounded offline runtime
load/release cycle, and a signed-out core with no saved accounts. These checks do
not claim that a game, purchase, chat, or physical controller was tested in the
distribution build.
