# Steam store, profile and game requirements

Implemented on 2026-10-09. The native UI embeds Steam's real websites; SkinOS does not reimplement checkout or issue purchase requests. Store/profile browsing uses an ephemeral website data store belonging to the current account.

## Session flow

SteamClient refresh tokens require an authenticated CM connection to mint web access tokens. The maintained primary implementation [node-steam-session, LoginSession.getWebCookies](https://github.com/DoctorMcKay/node-steam-session/blob/master/src/LoginSession.ts) documents that unauthenticated HTTP refresh for this token class fails with AccessDenied since 2025-04-30. [node-steam-user's webLogOn](https://github.com/DoctorMcKay/node-steam-user/blob/master/components/07-web.js) calls that flow over its existing client session.

SkinOS sends `Authentication.GenerateAccessTokenForApp#1` on the active CM session, using its matching SteamID and its own stored refresh token. `renewal_type = None` explicitly avoids rotating the long-lived token used by game launch. Request/response fields follow [Valve's protobuf definition tracked by SteamDatabase](https://github.com/SteamDatabase/Protobufs/blob/master/steam/steammessages_auth.steamclient.proto).

The returned access token becomes `steamLoginSecure = encodeURIComponent(steamid + "||" + access_token)`, accompanied by a cryptographically random `sessionid`. Cookies are individually scoped to `store.steampowered.com`, `steamcommunity.com`, and `checkout.steampowered.com`; no `.steampowered.com` parent-domain credential is created. A backend HTTP request to the requested Steam website must report the same signed-in account before credentials are emitted. JWT claims supply account/expiry consistency checks only; they are not considered local signature verification. Cookie lifetime is capped at six hours and at the token's actual expiration.

Backend verification follows at most three redirects, only within the same exact HTTPS host, and caps response size. A disposable HTTP cookie jar retains Steam's country/language/browser response cookies during verification; discarding those cookies caused a real store redirect loop and was corrected during the live check. The whole verification has a 45-second deadline. Credentials never appear in URLs, diagnostic output, or backend caches. `steamLoginSecure` is Secure/HttpOnly; `sessionid` is Secure and readable to Steam's own CSRF code. Cookies pass only through the existing private NDJSON connection to the native app. The frontend rejects other cookie domains, uses a nonpersistent WKWebsiteDataStore, and discards it on account changes/logout. Steam pages may set their own subsequent cookies normally within that isolated browser store.

IPC:

```text
get_web_session {target: "store" | "community"}
  -> web_session {steamid, target, url, expires_at, cookies[]}

cookie = {name, value, domain, path, secure, http_only, expires_at}
```

The daemon requires the current authenticated account, rejects unknown targets, and drops responses from an earlier login generation. Failure emits an ordinary `web_session_failed` error; it does not silently display another account or claim login succeeded. Callers must not log or persist the event payload.

## Game card details

`get_game_details {appid}` fetches the store's actual first-party [appdetails endpoint](https://store.steampowered.com/api/appdetails?appids=1942280&l=russian), returns the published Windows/macOS/Linux flags, and maps `pc_requirements`, `mac_requirements`, and `linux_requirements` into minimum/recommended plain text. Steam's empty-array representation for absent requirements is accepted. The publisher's platform declarations are distinct from SkinOS compatibility settings; NotProton does not turn a Windows requirement into a native macOS requirement. Valve documents platform configuration in [Steamworks: Platforms](https://partner.steamgames.com/doc/store/application/platforms).

```text
game_details = {
  appid, name, description, store_url, header_url,
  platforms: {windows, mac, linux},
  requirements: {
    windows: {minimum, recommended},
    mac: {minimum, recommended},
    linux: {minimum, recommended}
  }
}
```

Metadata responses have a 4 MiB bound, a 25-second HTTP timeout, and an in-memory cache of at most 128 games for one hour. Failed requests are not cached. Store descriptions and requirement markup are converted to bounded plaintext, excluding scripts/styles/embedded objects, without evaluating HTML or JavaScript. Empty requirements stay empty for the UI's “not specified” state. Header images are restricted to HTTPS Steam CDN hosts.

## Verification

Focused tests cover all three requirement formats, missing/empty requirements, HTML entities and script removal, text bounds, access-token account/expiry/audience checks, exact-host cookie isolation and redirect rejection, and server-confirmed login markers. Reqwest request/redirect/body APIs were checked against the [maintainer's primary documentation and tests](https://github.com/seanmonstar/reqwest).

Live integration passed using the actual saved SkinOS session: both store and community confirmed the matching account over HTTPS; Brotato returned requirements for Windows/macOS/Linux; Soundpad returned Windows requirements. The same core session loaded 46 friends with names, the current profile in invisible mode, and one read-only chat history. No messages were sent and no purchase was made. After logout, a new web session request was rejected. The test core exited cleanly.

Reproduce with `python3 scripts/check-store.py` after closing the GUI, to avoid competing CM sessions. Results are recorded in `outputs/store-social-check.json` as aggregate boolean/count metadata. Credentials, friend identities, message contents and authenticated page source are neither printed nor saved.
