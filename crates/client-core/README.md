# SkinOs core

Real Steam CM login and owned library, with SkinOs-only Keychain sessions. No Steam executable is spawned and no installed Steam credentials are imported.

Run `cargo run -p skinos-core`. stdin/stdout use one JSON object per line. stdout contains protocol events only.

## Protocol v1

Request: `{ "id": "unique-string", "command": "status", "payload": {} }`

Event: `{ "id": "request-id-if-applicable", "event": "status", "payload": {} }`

| Command | Payload | Behavior |
|---|---|---|
| status | `{}` | Current state and saved SkinOs accounts; does not access tokens |
| login_qr | `{}` | New real Steam QR challenge |
| login_password | `{account_name, password}` | Password is sent through Steam auth; `account` alias accepted |
| submit_guard | `{code}` | Email/device code for current sign-in |
| cancel_auth | `{}` | Cancel outstanding sign-in and close its connection |
| restore_session | `{steamid}` | Read only that SkinOs Keychain entry and authenticate |
| refresh_library | `{}` | Resolve current account's license packages and PICS data |
| logout | `{forget?:false}` | Disconnect; `forget:true` also deletes this saved SkinOs account/session |
| install_game / launch_game | Adapter-defined, including `appid` | Passed to compiled OperationAdapter; standalone core reports unavailable |
| quit | `{}` | Cancel workers, close connection, flush events, exit |

Key events:
- `ready`: protocol_version, backend, version.
- `auth_qr`: url, expires_in_seconds. Render QR locally; do not persist/log the URL.
- `guard_required`: kind = email_code / device_code / device_confirmation.
- `authenticated`: steamid (string), account_name, session_saved, generation.
- `library`: steamid, games, cached (true means local metadata, not current ownership proof).
- `error`: code, message, retryable. Failed library retrieval is never a successful empty library.
- `status`: state, optional accounts/current account.

Games have appid, name, playtime_minutes, last_played, app_type, installdir (metadata folder name), installed_dir (only a verified local installation path), macos_supported, header_url, launch. `macos_supported` means PICS has an explicit macOS launch entry; it does not assert that a game works on this Mac.

Launch entries preserve PICS fields: executable, arguments, workingdir, launch_type, oslist, osarch, betakey. Except executable, these may be null. Do not interpret command-line strings as shell programs.

## Storage and adapters

`session::data_dir()` defaults to `~/Library/Application Support/SkinOs`, overridable with `SKINOS_DATA_DIR`. Account registry and per-account library JSON are atomic writes. They contain no passwords/tokens. Refresh tokens live in Keychain service `dev.skinos.steam.refresh-token`, account key = SteamID string, accessed through Security.framework. `session::load_refresh_token` returns a non-Debug/non-Serialize secret wrapper.

`daemon::serve_with_adapter(Arc<dyn OperationAdapter>)` lets the composition binary attach download/runtime operations. Adapter receives Request, OperationContext (steamid, games, shared authenticated CM connection), and an event sender. Emit actual progress/completion; returning an error emits operation_failed. Session changes abort operation futures. The adapter must make partial file writes cancellation-safe and must not spawn detached mutation tasks.

Vendored steam-cm-protocol 0.4.1 preserves upstream MIT license. See vendor/steam-cm-protocol/SKINOS-PATCHES.md for local changes. Upstream full runtime/DRM functionality is not assumed.

## Verification boundaries

Unit and protocol tests can run without Steam credentials. Actual sign-in, ownership, downloads, and game identity require the user to complete the real sign-in in the app. A QR challenge alone is not a successful account login.

Local validation (2026-10-09): core tests and NDJSON startup/status/error/quit smoke passed. A live CM connection returned an actual Steam QR challenge; cancelling returned signed_out and exited cleanly. Missing saved-session restoration returned a retryable no_session error. No QR was scanned and no real account login/library was claimed by these checks.
