# Brotato macOS bundle: read-only diagnosis

Inspected 2026-10-09. AppID `1942280`, installed build `23429717`.
Bundle: `~/Library/Application Support/SkinOs/games/1942280/Brotato.app`.
This inspection did not launch the game, alter files or xattrs, re-sign code,
or change system security policy. The earlier executable-mode repair was
performed separately by the root agent.

## Conclusion

The downloaded app lacks a complete **bundle resource signature**, but its
Mach-O code-page signatures are intact. The `codesign` resource warning alone
does **not** establish why the game failed to launch, and does not establish
that the downloader omitted files. Runtime/LaunchServices failure evidence is
still required to identify that cause.

## Observed locally

- `Contents/Info.plist` is a valid plist with package type `APPL` and executable
  `Brotato`; that executable exists and is now mode `0755`.
- The executable contains both `x86_64` and `arm64`. `libsteam_api.dylib` exists
  beside it, matching the executable's `@loader_path/libsteam_api.dylib` load
  command. Other declared dependencies are system frameworks/libraries.
- `Contents/Resources` contains the game `.pck`, icon and localization files.
  `Contents/_CodeSignature/CodeResources` is absent.
- Both architectures report `Signature=adhoc`, no TeamIdentifier,
  `Info.plist=not bound`, and `Sealed Resources=none`.
- Their CodeDirectories contain two special slots; the resource-seal slot
  numbered `-3` is absent. Slot `-1` for Info.plist is zero.
- A separate, read-only Mach-O parser recalculated every code-page SHA-256:
  **8,147/8,147 x86_64 pages and 7,194/7,194 arm64 pages match**. This establishes
  code-page integrity, not developer identity, notarization, or resource trust.
- `codesign --verify --deep --strict --verbose=4 Brotato.app` fails with
  `code has no resources but signature indicates they must be present`.
  Explicit verification of either architecture produces the same result.
- `xattr` lists `com.apple.provenance` on the app and executable, but no
  `com.apple.quarantine`. Nothing was removed.

The installation team's separate Steam manifest validation covers 26 payload
files totaling 291,245,046 bytes. This inspection did not redownload or reparse
the remote manifest. The local install record identifies build `23429717`.

## Why the wording is misleading

Apple's open-source `SecStaticCode::loadResources` raises the same
`errSecCSResourcesNotFound` for either mismatch: resources on disk without a
seal, or a seal without a resource base. Therefore the message is not proof of
a missing resource file. The observed Brotato state matches the first case.
[Apple Security, StaticCode.cpp, loadResources](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_codesigning/lib/StaticCode.cpp#L1551).

The bundle seal normally lives in `CodeResources`; its hash is protected by
CodeDirectory slot `-3`. Here no such slot exists. There is consequently no
signed expected `CodeResources` hash from which to infer that the download lost
that file. [Apple TN3126, Resources](https://developer.apple.com/documentation/technotes/tn3126-inside-code-signing-hashes).

Apple distinguishes executable signing from distribution trust: an ad-hoc
signature can satisfy Apple-silicon executable-signing requirements, while
not establishing an identity accepted by Gatekeeper. A bundle verification
failure and a kernel executable-signature failure are different observations.
[Apple universal-app release notes, Code Signing](https://developer.apple.com/documentation/macos-release-notes/macos-big-sur-11_0_1-universal-apps-release-notes/).

## Next evidence needed

1. Capture the exact launch failure after the mode repair: `posix_spawn`/exec
   error versus a process that starts and exits. Preserve errno, exit status,
   signal, and stderr without credentials.
2. If the process is terminated before game code starts, correlate the attempt
   timestamp with a code-signing/security or dyld diagnostic. A generic Finder
   dialog is insufficient to distinguish these failure classes.
3. If Godot starts but Steam API initialization fails, investigate runtime IPC,
   account identity and the game environment. The bundle warning does not
   explain an SDK handshake failure by itself.
4. If the question becomes whether the depot intentionally ships without
   resource sealing, compare the exact same build/manifest installed by an
   independent downloader or official Steam. Do not infer all Steam depots have
   the same signing layout from this one app.

No signing repair or security-policy change is proposed by this diagnosis.
