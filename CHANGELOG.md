# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.0] - 2026-10-16

### Added

- Security screen: set the lock and auto-lock, manage webhooks, turn receipts on — via the system's admin prompt. @vhidvz
- In-app Guide covering every feature, and a self-ticking getting-started checklist on the Overview. @vhidvz
- Signed-receipt panel on results; Operations says whether the selected terminal's receipts are signed. @vhidvz
- Settings explains each data file and can roll back to the previous AppImage version. @vhidvz
- `posd admin --json`, `set-password --stdin` and `events enable|disable <id>`. @vhidvz
- `GET /api/v1/events` adds `eventTypes` and `adminFile` (path, world-readable). @vhidvz

### Fixed

- Blank window on hosts with a new Mesa: the AppImage no longer bundles libwayland; CI checks it. @vhidvz

## [1.0.0] - 2026-10-14

### Added

- initial release 🎉​🎊​.

[unreleased]: https://github.com/vhidvz/payment-pos-app/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/vhidvz/payment-pos-app/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/vhidvz/payment-pos-app/releases/tag/v1.0.0
