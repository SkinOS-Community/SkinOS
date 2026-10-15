Upstream: steam-cm-protocol 0.4.1 (MIT), crates.io source / published git 878cc7d0c2e458d238dcb54800b947b95665c97f.

SkinOs changes:
- Authentication task tracked/aborted on disconnect and Drop.
- LibraryError event distinguishes network failure from zero owned games.
- Preserve ClientLicenseList received before token-logon acknowledgement; local WebSocket regression test.
- Bound PICS access-token waits and handle explicit server ClientLoggedOff notifications.
- Connection accessor for same-session content adapters.
- Public binary KV parser for depot metadata.
- Depot key EMsg 5438/5439 (SteamTracking/Protobufs steam/enums_clientserver.proto).
- Device display name SkinOs; macOS sends EOSType::MacOSUnknown (-102) instead of Windows 11. Enum source: SteamRE/SteamKit SteamKit2/SteamKit2/Base/Generated/SteamLanguage.cs.

No bypass, credential import, or official Steam process launch is added.
