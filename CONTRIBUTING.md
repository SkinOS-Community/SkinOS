# Contributing to SkinOS

SkinOS is an independent macOS client built with Rust and SwiftUI/AppKit. Contributions to downloads, compatibility, accessibility, localization, documentation and reproducible bug reports are welcome. English and Russian reports are both fine.

Start with [the development guide](docs/development.md). You can build and test the source without a Steam account, CrossOver, signing keys or the proprietary game runtime.

## Find something to work on

- Use [Issues](https://github.com/SkinOS-Community/SkinOS/issues) for reproducible bugs and small, concrete feature requests.
- Use [Discussions](https://github.com/SkinOS-Community/SkinOS/discussions), when enabled, for support, design proposals and ideas that need discussion before implementation.
- `good first issue` identifies a bounded task with enough context for a new contributor. `help wanted` means maintainers welcome an implementation or investigation.
- Comment before taking a larger task, and open a draft pull request early when a design changes protocols, persistence, authentication, runtime boundaries or update delivery.

An issue is not a guarantee that a feature will be implemented. Maintainers record scope and decisions in the issue or pull request so others can follow the reasoning.

## Make a change

1. Fork the repository and create a branch for one coherent change.
2. Keep third-party sources and their license notices separate from SkinOS changes. Record the upstream revision and explain local patches.
3. Add regression coverage for changed behavior where useful. Offline fixtures must be synthetic or explicitly public, with their source recorded.
4. Run the relevant checks in [the development guide](docs/development.md). State the commands, results and any checks you could not run.
5. Open a pull request describing the problem, resulting behavior and validation. Include screenshots for visible UI changes, with account details and chat content removed.

Do not include generated applications, game files, personal runtime caches, Steam credentials, signing identities or private diagnostics. Review your diff and new files before uploading. Diagnostic redaction is a useful first pass; inspect the result yourself before sharing it.

Original SkinOS contributions use [GPL-3.0](LICENSE). Only submit material you have the right to contribute, and retain the license and attribution for third-party material. The repository license does not relicense Valve, CrossOver or other separately supplied components.

## Review and compatibility

Reviews should explain a concrete correctness, maintainability or user-facing concern. Prefer focused patches and a regression test over unrelated cleanup. Maintainers may request a smaller scope or defer changes that cannot yet be validated.

A successful compile is not evidence that a game, controller or Steam service works. Reports should distinguish offline tests, real account/network tests and tests with physical hardware. Include the game AppID, macOS and CrossOver versions, and runtime selection when relevant. Do not claim complete Steam feature parity or support for every game.

Steam Input remains experimental; physical-controller coverage is limited. Changes involving controller input, game launch or Wine need an explicitly described manual test in addition to offline tests when the required hardware/runtime is available.

Report vulnerabilities through [SECURITY.md](SECURITY.md), and follow the [Code of Conduct](CODE_OF_CONDUCT.md).
