# Friends, profile and chat integration

Verified 2026-10-09 against the current protocol sources and the locally vendored client.

## Protocol choices

- Steam's persona enum defines Invisible as value 7. SkinOS sends that as its first status on every connection; it never sends an intermediate Online status. The runtime host must independently follow the selected presence because it is a separate Steam session. [Valve ISteamFriends documentation](https://partner.steamgames.com/doc/api/ISteamFriends#EPersonaState)
- The client changes persona using `ClientChangeStatus`, and reads account and friend personas through `ClientRequestFriendData`. Friendship changes are incremental, so a removal must remove only its corresponding row. Partial persona updates must retain fields that were absent. The wire decoder preserves these distinctions. [node-steam-user implementation](https://github.com/DoctorMcKay/node-steam-user/blob/master/components/friends.js)
- Private text chat uses the authenticated unified `FriendMessages.SendMessage#1` and `FriendMessages.GetRecentMessages#1` services. Incoming notifications use `FriendMessagesClient.IncomingMessage#1`. The response timestamp and ordinal identify acknowledged messages, and `local_echo` identifies outgoing cross-session echoes. History is currently the 50 most recent messages, ordered oldest first. The sender's account ID identifies message direction. [Steam protocol definitions](https://github.com/SteamTracking/Protobufs/blob/master/steam/steammessages_friendmessages.steamclient.proto)

## SkinOS API

Commands: `get_friends`, `get_profile`, `set_presence`, `get_chat_history`, `send_message`.

The last two commands require a current friend SteamID. `send_message` additionally takes text and an optional client message ID. Its result is emitted only after a successful Steam service response. Errors and timeouts retain the request ID; sends are never automatically retried. No messages or friend identities are cached to disk. Every worker and response belongs to a single session generation; logout/account switching discards all previous social state.

Events: `friends`, `friend_update`, `profile`, `presence`, `chat_history`, `chat_message`, `chat_typing`, and explicit `error` responses. SteamIDs are strings throughout the UI protocol. Presence supports invisible, online, away, busy and offline.

The account profile is filled from Steam's persona data. The login account name is only the initial fallback while the profile data is loading.

## Verification

- Local WebSocket protocol tests confirm that the first outgoing persona frame is Invisible with no subsequent unsolicited Online frame.
- A mocked Steam rejection with result 15 is returned as an error, never as a sent message. A successful response retains the server timestamp and ordinal.
- A logon test sends licenses and a friends list before the login acknowledgement; neither gets discarded.
- State tests cover incremental friend removal, partial persona updates, account isolation, invalid/empty/oversized messages and echo/ack deduplication.
- All 37 protocol and 23 core tests passed, and strict Clippy passed on 2026-10-09.
- No messages were sent to the user's real friends for testing. The send path is tested using a local mock Steam WebSocket server.
