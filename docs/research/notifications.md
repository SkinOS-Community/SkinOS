# Steam notifications — 2026-10-10

SkinOS uses the real authenticated CM connection for the read-only `SteamNotification.GetSteamNotifications#1` method. Its published request has no paging/count-limit field. SkinOS therefore bounds the call to 12 seconds, accepts at most 2 MiB of encoded response, and retains the latest 200 records. There are no calls to MarkRead, MarkViewed, Hide or SetPreferences. [Steam service definitions](https://github.com/SteamTracking/Protobufs/blob/master/webui/service_steamnotification.proto)

Live changes come from `SteamNotificationClient.NotificationsReceived#1`; the decoder also preserves pending gift, friend and family-invite counts. Unknown positive types use a generic Steam notification and a safe Steam destination. Initial data fills the inbox silently. A newly received unread record can request a desktop alert; incremental events precede the inbox snapshot. [CM push schema and type enum](https://github.com/SteamTracking/Protobufs/blob/master/steam/steammessages_notifications.steamclient.proto), [independent client implementation](https://github.com/DoctorMcKay/node-steam-user/blob/master/components/notifications.js)

Friend requests also use genuine incoming `ClientFriendsList` relationship changes. DMs come from the existing real-time FriendMessages path; no chat history is fetched for notifications. Incoming DMs use exactly the same ID as their `chat_message` event. Self echoes and history/replays do not trigger desktop alerts. Generic Steam message/friend-invite records do not create a second desktop alert for these same channels.

UI commands/events:

- `get_notifications {}` returns correlated `steam_notifications`.
- `steam_notifications`: account SteamID string, notification rows, unread count, initial flag, pending gift/friend/family counts.
- `steam_notification`: account SteamID, one normalized row, `is_new`, `notify`. The desktop layer posts only when `notify` is true.
- `steam_notifications_status`: account SteamID, `ready`/`unavailable`/`degraded` state, optional code/message and retryability. Background notification failures use this feature-scoped event, never a global game error. Explicit refresh failures remain correlated errors.
- Rows: ID, numeric Steam type, category, bounded plain title/body, optional Steam-only URL/AppID/friend SteamID, timestamp and read flag.

The initial notification fetch (including early UI refresh waiters) waits until the first library load settles, so initial PICS requests do not consume its response deadline. Real-time pushes populate the inbox immediately. Failed refreshes retain the existing inbox and can retry on the next 90-second poll or manual refresh.

Refresh/push handling belongs to the current session generation. Account-scoped metadata persists only bounded deduplication IDs and pending-request identities, with private filesystem permissions; bodies and URLs are not persisted. HTML/control characters are removed, URLs are restricted to HTTPS Steam hosts and stripped of queries/fragments, and Steam Guard/confirmation bodies remain generic.

Verification: 6 focused core tests cover baseline silence, push ordering, persistent/account-bound deduplication, DM echo/history handling, friend requests, payload limits and feature-scoped storage/transport failure behavior. Two local-WebSocket protocol tests cover the actual read-only request and bounded push decoding. No messages, read-state changes, purchases or notifications were sent to a real account during these tests.
