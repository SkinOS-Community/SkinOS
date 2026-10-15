# MVP process protocol

UTF-8 NDJSON over the bundled `skinos-core` child's stdin/stdout. Each request is `{ "id": "unique-string", "command": "name", "payload": {} }`; each event is `{ "id": "request-id-if-applicable", "event": "name", "payload": {} }`. Request IDs must be unique within the UI session. Diagnostics use stderr and must not contain passwords, guard codes, refresh tokens or web-session cookies.

## Commands and event groups

- Session: `status`, `login_qr`, `login_password`, `submit_guard`, `cancel_auth`, `restore_session`, `logout`, `refresh_library`, `quit`.
- Library/runtime: `install_game`, `update_game`, `check_updates`, `launch_game`, `stop_game`, `get_game_log`.
- Download queue: `get_download_queue`, `pause_download`, `resume_download`, `cancel_download`, `remove_download`, `move_download`, `set_download_limit`.
- Settings: `get_settings`, `save_settings`, `get_game_settings`, `save_game_settings`.
- Social: `get_friends`, `get_profile`, `set_presence`, `get_chat_history`, `send_message`.
- Store: `get_game_details`, `get_web_session`.

Core events include `ready`, `status`, `auth_qr`, `guard_required`, `authenticated`, `library`, `error`, `download_enqueued`, `download_queue`, `install_progress`, `install_complete`, `launch_status`, `stop_requested`, and `operation_complete`. Social, store, settings and diagnostic responses use their corresponding named events.

`operation_complete` for `launch_game` is emitted only after the operation and runtime-host cleanup finish. Consumers must not stop the core solely on an intermediate `failed` or `exited` launch status: its private IPC service may still be cleaning up. `stop_game` acknowledges delivery through `stop_requested`; completion of that request does **not** mean the game has exited. The original launch request continues to emit the game's states and final completion/error.

## Download queue

`install_game` and `update_game` take `{ "appid": 123 }` and enqueue work. `download_enqueued` acknowledges acceptance with `appid` and `queue_id`; it is not installation success. Successful installation later emits `install_complete`, the updated library, a `completed` queue row, then `operation_complete`. A failed job emits a `failed` row with its error and a correlated `download_failed` error. Pause and cancellation are expected state transitions, not download failures.

| Command | Payload | Meaning |
|---|---|---|
| `get_download_queue` | `{}` | Return the current account's snapshot. |
| `pause_download` | `{ "appid": 123 }` | Interrupt an active attempt or pause a queued item. |
| `resume_download` | `{ "appid": 123 }` | Queue a paused, cancelled or failed item again. |
| `cancel_download` | `{ "appid": 123 }` | Cancel queued/active work; preserve resumable staging and installed games. |
| `remove_download` | `{ "appid": 123 }` | Remove the queue record; active work is safely stopped/drained first. Game files and staging are retained. |
| `move_download` | `{ "appid": 123, "position": 0 }` | Move a **queued** row to a zero-based position in the full items array. |
| `set_download_limit` | `{ "limit": 2 }` | Set 1–4 concurrent games; default 2. |

Successful controls emit a `download_queue` snapshot with the control request's `id`. Asynchronous progress snapshots have no request ID. For `remove_download`, the correlated snapshot is deferred until the row has actually disappeared and that removal has been saved. Intermediate snapshots while the worker stops have no request ID; the UI should keep the removal action pending until the final acknowledgement or correlated error. Lowering the limit or reordering never interrupts active games; queued work follows the new order as slots become available. Jobs targeting a running game or an occupied installation directory wait for that resource.

`download_queue.payload` contains:

```json
{
  "steamid": "SteamID64-as-string",
  "revision": 12,
  "max_concurrent": 2,
  "items": [
    {
      "appid": 123,
      "state": "downloading",
      "queue_position": 0,
      "is_update": false,
      "progress": 0.25,
      "downloaded_bytes": 1000000,
      "total_bytes": 4000000,
      "bytes_per_second": 500000,
      "eta_seconds": 6,
      "reused_bytes": 2000000,
      "processed_bytes": 3000000,
      "install_total_bytes": 6000000,
      "files_completed": 4,
      "total_files": 10,
      "chunks_completed": 6,
      "total_chunks": 20,
      "message": "Загрузка файлов",
      "error": null,
      "remove_requested": false
    }
  ]
}
```

Possible states: `queued`, `preparing`, `verifying`, `downloading`, `installing`, `pausing`, `paused`, `cancelling`, `cancelled`, `completed`, `failed`. `progress`, `eta_seconds`, `message` and `error` may be null. `queue_position` matches the supplied array order, including terminal rows.

The UI must match `steamid` against the authenticated account and accept only newer revisions within the current core/session generation. Clear the previous generation's queue on logout/disconnect. The snapshot is authoritative; do not independently overwrite its rows with legacy `install_progress` events. Legacy progress is still emitted for older consumers and contains the same measurements for the **current attempt**. Queue network counters accumulate actual bytes across pause/resume attempts, while verification uses `processed_bytes / install_total_bytes` and file/chunk counts. Unknown new network totals during preparation retain the prior counters.

`pausing`/`cancelling` acknowledge an interruption request. The slot is released only once the attempt returns and its worker is joined; the row then becomes `paused`/`cancelled`. If the short atomic installation commit has already started, pause/cancel is explicitly rejected and the install finishes. Cancellation never commits a not-yet-committing attempt or deletes an installed game. Resume revalidates saved partial data; counters alone never authorize reuse.

Removing a terminal, paused or queued row updates only queue metadata immediately. Removing an active row first persists a removal intent and requests cancellation; the row stays visible until its worker is joined. If an atomic installation commit already started, removal waits for it to finish and then removes the record while preserving the installed game. `remove_requested` is true during this process, and conflicting controls are rejected. If saving the final removal fails, the stopped row remains visible and the command receives an error; removing the row can be retried. Delayed events from an old attempt cannot resurrect a removed row or affect a newly queued attempt for the same AppID.

Queue intent and numeric counters are stored per account under `SkinOs/download-queues/<steamid>.json`, without credentials, connection objects, error URLs or chat data. Restart restores unfinished work as paused; an accepted `cancelling` intent restores as cancelled. A persisted `remove_requested` intent is finalized by removing only that record on restart. Logout and account switching interrupt/drain the old queue before disconnecting it. A different account cannot execute or load that account's queue file.

## Other core payloads

### Local uninstall

`get_uninstall_plan {appid}` returns correlated `uninstall_plan` with `appid`, `install_dir`, `additional_install_dirs`, `partial_dirs`, `game_bytes`, `partial_bytes`, `remove_bytes`, `shared_appids`, and `confirmation_token`. Preparation does not remove installation data. The UI displays the paths and permanent-deletion scope before confirmation. A nonempty `shared_appids` means the primary Steam directory remains; additional legacy copies and own partials are still included in removal.

`uninstall_game {appid, confirmation_token}` executes only the approved server-side plan. Tokens are single use, expire after 15 minutes, and match the account, AppID and current library. Filesystem changes require a fresh preview. Running games or active download workers block the operation; idle queue records are removed before the filesystem transaction. On success, correlated `game_uninstalled` includes `appid`, `install_dir`, `shared_directory_retained`, `removed_bytes`, `removed_partial_dirs`, followed by a refreshed `library` and `operation_complete`. On filesystem failure, library facts refresh and `error` identifies the failure; no success is emitted. No arbitrary deletion path is accepted from the UI.

### General events

- `auth_qr`: `url`; the UI renders the QR locally.
- `authenticated`: `account_name`, `steamid` string, `session_saved`, `generation`. Persona information arrives separately in `profile`.
- `library`: `steamid`, `games`, `cached`. Games include AppID, name, playtime, installed path/build, platform support, launch metadata and artwork URL.
- `error`: `code`, `message`, `retryable`; correlated responses include the request ID.
- `install_complete`: `appid`, `install_dir`, `build_id`. The default library is `$HOME/Library/Application Support/Steam/steamapps`, configurable in settings.
- `launch_game` / `stop_game`: `appid`.
- `launch_status`: `appid`, `state`, optional `message` and account identity. A successful SDK handshake is not proof that the game survived; a subsequent crash emits `failed` and a terminal error.

Refresh tokens stay in the backend's configured credential store (private disk storage for development, or Keychain). Web-session cookies are delivered only for the owned embedded webview and must not be logged. Game/account identity is checked in the core rather than inferred from the current visual selection. Unsupported commands return an explicit error.
