# Security reports

Do not publish exploitable vulnerabilities, credentials or private download URLs
in a public issue. Use GitHub's **Report a vulnerability** option in this
repository's Security tab when private reporting is enabled. Otherwise contact
the repository owner privately using the contact route on their GitHub profile;
if none is available, request a private reporting channel without sharing the
vulnerability details. A dedicated security email is not configured yet.

Include the affected version, OS, reproduction steps, impact and a minimal
sanitized example. Give maintainers an opportunity to investigate and coordinate
a fix before public disclosure. No guaranteed response deadline or bounty
program is offered. Private reporting must be enabled by the repository owner
before the first production release.

Only the latest released version is considered for security fixes. Pre-1.0
builds may change profile and API compatibility; no long-term support branch
exists. TLS verification, explicit credential handoff, origin-scoped sessions,
bounded resources and authenticated local APIs are security boundaries.

Release payloads and update metadata must be verified against an embedded public
key. Unsigned checksums detect corruption but do not authenticate a publisher.
Platform signing and notarization are separate checks. Never execute downloaded
files automatically, put signing keys in this repository, or disable certificate
validation to work around a connection problem.
