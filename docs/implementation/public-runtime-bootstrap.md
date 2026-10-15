# Publisher-hosted runtime bootstrap

Verified 2026-10-15. No Steam account, credential store, or game was opened. Downloaded archives were read as data; no downloaded binary was executed.

## Confirmed result

The existing macOS runtime is reproducible without installing or starting the original Steam GUI. Seven historical archives are still served by Valve. Their archive sizes/SHA-256 hashes were verified, and 3,812 selected members match every corresponding entry in `runtime-payload-pin.json` byte for byte. The remaining file is deterministic SkinOS metadata, also regenerated with its original hash. The existing engine fingerprint stays `035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`.

The public DMG can contain SkinOS code, updater, open-source integration helpers and immutable metadata only. The first launch downloads required proprietary components directly from their publishers into the application data directory; source distribution and GitHub releases do not need to mirror those bytes. This changes delivery, not runtime behavior or the audited private ABI.

## Exact macOS sources

Source for the historical package coordinates was the generic `steam_client_signed-2_osx.manifest` in an installed Steam package cache, manifest version `1788652215`. Every listed ZIP was then independently fetched from the public HTTPS endpoint below, so installation/cache presence is not required to bootstrap another Mac. The current [Valve manifest](https://client-update.akamai.steamstatic.com/steam_client_osx) reports a newer version; never substitute latest packages for these pins.

| Package | Compressed bytes | Pinned files | Archive SHA-256 |
| --- | ---: | ---: | --- |
| [bins_client_osx](https://client-update.akamai.steamstatic.com/bins_client_osx.zip.f7656139b237bfef41c68b8d2e28f6eead50fed2) | 18,575,968 | 1 | `9dd984c379df9abe2b9d0855f0d797109ab2b06276a43cb14f0d673f69f8efd4` |
| [bins_osx](https://client-update.akamai.steamstatic.com/bins_osx.zip.40e90e639beb889e771c2669d274e065623c6fb8) | 39,393,202 | 6 | `8f8b075397f5b933bf7dcf9d6ae611487016f8554592edaa28f1589fbba2c152` |
| [breakpad_osx](https://client-update.akamai.steamstatic.com/breakpad_osx.zip.3a5f76a13e79733ae3b08af8c94dafba2f99d2a5) | 576,088 | 38 | `56eefbca21e1adaf255160744aa9aa05fb31d153ce537d4b220cfd6169a94ce5` |
| [public_all](https://client-update.akamai.steamstatic.com/public_all.zip.4909469e2596e887494f0882fc39a493ac6c6925) | 31,197,822 | 3,616 | `e8a1c9234c44e513d9c4ac094168b96bfa9e39b02fab8683cd793d2509f5ea94` |
| [strings_all](https://client-update.akamai.steamstatic.com/strings_all.zip.9c6cdbadbbf194703f40a5eef735ce7b99eb10dc) | 4,014,695 | 145 | `23e192629927ca1fcfda167b3cf55ed69fd212515fc0547eee268325b24d41a4` |
| [strings_en_all](https://client-update.akamai.steamstatic.com/strings_en_all.zip.071a3f6839c3b8d574731101f2862d5ed72ab2e4) | 114,329 | 5 | `fe73301f09a423c3114575b54b929c5cc0d1aacc80f1067259e551ffd1c82a07` |
| [bins_misc_osx](https://client-update.akamai.steamstatic.com/bins_misc_osx.zip.7450dcbc9ce6066dd102f5ec175cea2ff4c89144) | 4,520,178 | 1 | `bb9ba9579676a61cc0a4bec476d85a14b364ca129ea0f94cc19dcaa466de871e` |

Total macOS download: **98,392,282 bytes**. The exact URL, archive pin and selected member list are machine-readable in `crates/runtime-host/resources/runtime-bootstrap.json`. No full ZIP tree is extracted.

The fixed four Breakpad symlinks are created from the existing audited runtime pin, never accepted from arbitrary archive contents. The deterministic `steam-input-source.json` is 492,622 bytes with SHA-256 `d35afc4db6cf23369ce02972b26e50a296631df29d53f80155666cc096c105af`. Its legacy `source_kind` describes the originally audited cohort; the actual delivery method is recorded as `publisher_pinned_downloads` in the new `current.json`.

## Exact NotProton sources

All 31 bridge/font files in the existing NotProton pin were independently recovered from these three publisher-hosted archives; archive hashes and all per-file hashes matched. NotProton remains version 1.1.7, commit `4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8`.

| Source | Compressed bytes | Pinned outputs | Archive SHA-256 |
| --- | ---: | ---: | --- |
| [notproton](https://github.com/NotProtonNot/NotProton/releases/download/v1.1.7/NotProton.zip) | 73,002,872 | 26 | `df116f5185fb57b6f6f8ff5f593b9851edb697a01777e403e1fd23185f9bdbff` |
| [linux](https://client-update.akamai.steamstatic.com/bins_misc_ubuntu12.zip.3f92810725ee673827371a0470cd4f8c7ea8cfae) | 37,703,386 | 3 | `026b984726c728bbf81ae3ba16623bcd96e2bf837a859c235acf0ee279d940fd` |
| [win64](https://client-update.akamai.steamstatic.com/bins_win64.zip.36f5d9202e79ab2aa3e3c5902e84bbd799d31fc0) | 63,700,191 | 2 | `93f5b6bea0267fd85dc8cc823fdab5c5fb55d7f3a1deab0598acefef0e133bce` |

The upstream source of the Valve bridge coordinates is [NotProton’s pinned valve-packages.manifest](https://github.com/NotProtonNot/NotProton/blob/4e8e0556e4972402bf99af4899ea4dc5b3bcd5a8/app/Sources/NotProtonApp/Resources/valve-packages.manifest). The 26 NotProton release outputs include bridge implementations and fonts; three selected DLLs come from the Linux package and two from the Win64 package. Member paths remain explicit allowlists; there is no installed Steam fallback.

## Implementation contract

- `skinos-dependency-assets::ArchiveSpec { url, size, sha256 }` identifies one immutable download. `ensure_archive(cache_root, spec)` validates publisher HTTPS hosts and redirects, writes a bounded stream to a private temporary file, verifies exact length/SHA-256, and atomically publishes a content-addressed cache entry. Existing corrupt/redirected cache entries fail explicitly.
- `PinnedZip::open(path)` and `read_member(name, sha256, size)` return only one named regular member after bounds/hash checks. No archive path is joined to an output root by the ZIP layer. Output construction uses the separate audited runtime manifest.
- `runtime_bundle::prepare` keeps the existing cache lock, full-tree validation, staged directory and atomic publication. A bundled/private payload still works. An absent payload, or the public build's exact compiled manifest without `payload.gz`, selects direct publisher downloads. A different manifest or present malformed/redirected archive fails instead of being silently replaced.
- A fresh cache is written below the requested `SKINOS_DATA_DIR`; normal account or credential data is neither read nor copied by provisioning. Download code does not call Xcode tools, Python, an installed Steam client, or a game.
- Runtime packages cannot be upgraded by an external moving manifest. An upgrade requires reviewing new package/member hashes and auditing the new engine fingerprint in source.
- Network and extraction helpers are blocking; async callers must dispatch them to their blocking worker.
- Provisioning finishes before the runtime worker is spawned and its 15-second offline-probe watchdog starts. Production launch waits for that host asynchronously. A distribution checker must give the separate first `--prepare-bundled-runtime` invocation enough download time; it need not extend the runtime-execution watchdog.

## Verification scope

Archive availability, complete hash equivalence, and deterministic metadata regeneration are confirmed. These findings concern delivery of exactly the existing runtime, not a new claim about physical-controller support or compatibility with a newer Steam engine. First-use network errors must leave staged state unpublished and a subsequent attempt can reuse already verified archive downloads.

The implemented runtime-host CLI was then run against a fresh isolated data directory, with a system-only `PATH` and an intentionally unavailable `DEVELOPER_DIR`. It downloaded all seven archives, provisioned and independently verified all 3,813 files and four symlinks, and reused the same unchanged cache on a second run. Both runs exited successfully. No runtime binary was loaded or executed. Local evidence is `outputs/public-runtime-bootstrap-1/summary.json` (excluded from publication). The helper's four offline tests and the runtime inventory/safety tests pass; strict Clippy also passes for these changes.

The original full private payload and user data were preserved. Public distribution should omit those payload bytes and use the slim/bootstrap path. Component licenses and source notices remain required for the code actually shipped; direct publisher acquisition does not relicense proprietary components.
