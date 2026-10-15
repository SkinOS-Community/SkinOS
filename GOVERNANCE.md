# Governance

SkinOS is community driven: bugs, roadmap proposals and implementation discussions belong in the public [SkinOS-Community/SkinOS repository](https://github.com/SkinOS-Community/SkinOS). It is an independent project and is not endorsed by Valve or CodeWeavers.

## Responsibilities

The repository owner is the initial maintainer and is accountable for scope, access permissions and releases. Contributors can propose changes, review code, reproduce issues and improve documentation without becoming maintainers.

Maintainers review changes and explain acceptance, rejection or deferral in the related issue or pull request. If reviewers disagree, they first try a smaller proposal or a reproducible experiment. The repository owner resolves an outstanding decision and records the reason publicly, except for confidential security reports or moderation details.

There is no promise of a fixed response time or implementation date. Roadmap entries describe intent; release notes describe what shipped.

## Becoming a maintainer

Consistent contributions, constructive reviews and demonstrated care for users' data are the basis for additional access. An existing maintainer proposes a specific responsibility publicly and obtains the contributor's agreement. Start with the smallest role needed, such as issue triage or review of one area. Repository administration and release credentials are separate responsibilities and are not automatically granted with code access.

Maintainers can step down at any time. Access should be removed when responsibilities end. Changes to this document go through a pull request, with time for community feedback before merging a substantive governance change.

## Releases and updates

Normal pull-request CI compiles and tests source without production signing credentials, account sessions or proprietary runtime payloads. Passing CI does not create or publish a release.

A designated release maintainer reviews the final source revision and dependencies, builds the distributable, signs and notarizes it, and validates the exact packaged app in a fresh data directory. Release notes should identify changes, compatibility requirements and remaining limitations. Checksums, update metadata and release assets must refer to that same version.

Developer ID, notarization and update-signing keys stay outside the repository and outside untrusted pull-request jobs. Changes to update verification, payload pins, signing, or release workflows require particular attention in review. Do not publish unreviewed pull-request artifacts as official updates.

## Community organization

Recommended labels are `bug`, `enhancement`, `documentation`, `good first issue`, `help wanted`, `needs reproduction` and `blocked`. Area labels can be added as the volume grows. A `good first issue` should describe expected behavior, likely files and a way to check the result.

Discussions should use a small set of categories: Announcements, Help, Ideas and Show and tell. Maintain a short roadmap that links to actionable issues instead of duplicating their status. Repository settings and labels are configured separately; this document does not imply they have already been enabled.
