# Steam download-region catalog

`download-regions.json` is a snapshot of 186 real Valve client cell IDs, paired
with Valve's Russian and English labels. It was extracted on 2026-10-14. IDs
are not inferred from server hostnames, IP geolocation, or display-name order.
ID 0 is deliberately absent: automatic selection is an application setting,
not a region in this catalog.

## Primary source and extraction

- The public Steam app-info cache, `appcache/appinfo.vdf`, format `0x07564429`.
  Only app 7's `client_cell_list` object was extracted. Its entries contain
  numeric cell keys and a `loc_name` string. The cached file's modification
  time was 2026-10-08 20:32:52 UTC.
- Valve's shipped `public/steamui_russian.txt` and
  `public/steamui_english.txt`: the exact `DownloadRegion_<loc_name>` labels.
  Both languages cover all 186 entries; no labels were invented. These files
  had modification times 2026-06-04 21:05:52 UTC and
  2026-05-29 19:33:30 UTC respectively.
- Valve's shipped `steamui/chunk~2dcc5aaf7.js` confirms that the downloads
  setting uses `vecValidDownloadRegions` entries with `nRegionID` and
  `strRegionName`, stored as the `download_region` setting.

These are public product resources from the installed Steam client. No account
configuration, login records, credentials, or user-specific data were exported.
The resulting catalog has unique, positive IDs and is sorted numerically.

For a refresh, read the app-info string table from its 64-bit offset at byte 8,
walk the app records from byte 16 (`appid`, `size`, then `size` bytes), and find
app 7. Parse its binary-KeyValues `client_cell_list` object using indexed key
names from that string table. Join each `loc_name` to the two localization
files. Valve's text format permits no whitespace between adjacent quoted key
and value strings, so the localization parser must accept that syntax.

Verification hashes (SHA-256):

- Canonical `client_cell_list` JSON, sorted keys, compact separators:
  `47a95fe7cdf80ba5c39fdcd1da17c2a6fd425435192389e8c24f3f0471e8284d`
- Russian localization file:
  `28a95952c793d01ecb213f35931d73a486c0f707f6f39be67e6d0256b737894b`
- English localization file:
  `e3b8377ddd57ebf76259acae0d48ea74b0edd1b0fc1dfe902b9995d2c512a083`
- Generated `download-regions.json`:
  `9408ea901baadad24567fbbd548a220a7ecb756712fb9a6424454e725336246e`

## Download-service semantics

The selected ID is a **preferred Steam content cell**, passed as `cell_id` to
`ContentServerDirectory.GetServersForSteamPipe`. It selects Valve's candidate
server pool; it is not a guarantee of one physical host or a country boundary.
Automatic mode should use the current successful login response's `cell_id`,
falling back to zero when unavailable. It should not alter the account's store
country or the connection-manager server list.

This matches SteamKit's current implementation and documentation, checked on
2026-10-14:

- [SteamContent.GetServersForSteamPipe](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Steam/Handlers/SteamContent/SteamContent.cs):
  explicit preferred cell, otherwise `Client.CellID ?? 0`.
- [CMClient.CellID](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Steam/CMClient.cs):
  Steam's recommended cell from the successful login response.
- [ContentServerDirectoryService](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Steam/WebAPI/ContentServerDirectoryService.cs):
  the Web API alternative, and conversion of Valve's server list.
- [DepotDownloader CDNClientPool](https://github.com/SteamRE/DepotDownloader/blob/master/DepotDownloader/CDNClientPool.cs):
  filters compatible content servers and app restrictions, orders by penalties
  and weighted load, and moves to another server after failure.
- [Valve support: download regions](https://help.steampowered.com/en/faqs/view/0805-81BF-E4E1-E222):
  confirms the user-facing download-region setting and trying another region
  when content servers are unavailable.

This is a bundled snapshot. New or renamed regions require refreshing the
catalog; server endpoints themselves must continue to come from Valve at
runtime.

## SkinOS integration

The current CM session model does not expose the logon cell. Automatic mode
therefore retains the existing `cell_id=0`, `max_servers=20` request. An explicit
region sends that verified cell ID with `max_servers=100` and prioritizes
eligible responses whose `Server.cell_id` matches it, followed by weighted
load. Valve-issued servers from other cells remain available as fallbacks.
The six-server preparation budget reserves up to four preferred candidates and
two fallbacks; unused slots are filled from the other group. Preferred candidates
are attempted first. This keeps fallbacks available even with many matching hosts.

This ranking matters: a live directory check on 2026-10-14 returned a Warsaw
pool first even when Frankfurt was requested; the larger response included
32 Frankfurt servers and 12 Warsaw servers. Passing a different ID alone did
not change the first candidates in that response.
