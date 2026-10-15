# Security policy

Security fixes target the latest published release. Older versions may require an update; no separate maintenance branch is promised during the 0.x development phase.

## Report a vulnerability privately

Use [Security → Report a vulnerability](https://github.com/SkinOS-Community/SkinOS/security/advisories/new) when private reporting is enabled. Include the affected version, a minimal reproduction and the expected impact. Use a test account or synthetic data whenever possible.

If that button is unavailable, open an issue that only requests a private reporting channel. Do not include exploit details, credentials or another person's data in that issue. There is no public security inbox or response-time guarantee until maintainers explicitly publish one here.

Useful reports include account-session disclosure, unsafe file writes or deletion, downloaded payload verification failures, update verification failures and unintended execution across a runtime boundary. Test only systems and accounts you own or have permission to assess. Do not send unsolicited messages to other Steam users or access their private data as part of a test.

## What not to upload

Never attach Steam passwords, Guard/recovery codes, QR login challenges, refresh/access tokens, cookies, authentication tickets, Keychain exports, signing keys, full account directories or private chat history. Remove Steam IDs, account names, personal paths and IP addresses unless necessary and shared privately by agreement.

SkinOS diagnostics attempt to redact secrets, but this is not a guarantee that arbitrary logs are safe to publish. Inspect excerpts before sharing them. If a credential is accidentally disclosed, revoke it through the relevant service and notify maintainers privately so exposed content can be removed.

## Builds and credentials

Debug builds can store refresh tokens on disk for development. Release builds default to Keychain; an explicit configuration override can change that. See [development notes](docs/development.md) before using a real account in a local build.

Pull-request CI must not receive signing keys, notarization credentials, Steam sessions or release write access. Release artifacts and update metadata must be validated together before publication. A source license does not grant rights to redistribute third-party runtime or game files.
