# Dota 2 public metadata fixture

`dota2-public-appinfo.json` contains only AppID 570's public common and depot
metadata, extracted on 2026-10-14 from Valve Steam's local appinfo cache (cache
dated 2026-10-08). Account, package, token, ownership and user records are absent.

The fixture preserves integer-versus-string KeyValues types. Depot 373307 has
`config.lowviolence = 1`, with no platform, DLC, language or shared-app filter.
The default installer must exclude this optional content before requesting its
depot key. The other metadata retains neutral content, OS-specific content,
shared redistributables, language packs and optional DLC for selection replay.

The upstream reference for the low-violence opt-in semantics is
[SteamRE/DepotDownloader ContentDownloader.cs](https://github.com/SteamRE/DepotDownloader/blob/master/DepotDownloader/ContentDownloader.cs).
