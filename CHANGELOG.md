# Changelog

Release sections use `## [MAJOR.MINOR.PATCH]` followed by Added, Changed, Fixed
or Security entries. Release automation requires an exact matching section.

## [0.1.0]

### Added

- Crash-safe HTTP/HTTPS downloads with durable checkpoints and SHA-256 verification.
- Desktop interface, CLI, authenticated daemon API and shared browser extensions.
- Adaptive scheduling, bandwidth limits and recurring traffic schedules.
- BitTorrent, magnet links, FTP, explicit FTPS and SFTP transfers.
- HLS and DASH VOD selection and crash-safe assembly for supported formats.
- Installer configurations, draft release automation and manually controlled signed updates.

### Release qualification

- Platform, browser and installer verification varies. Read the artifact labels
  and installation guide; a successful build is not a completed installer test.
- Live media, DRM handling, media remuxing, mobile, Flatpak and plugins are outside this release.
