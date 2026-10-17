# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Only `posd` serves the API; the console binds no port and installs, starts and restarts posd as a user service. @vhidvz
- An AppImage installs itself on first run; "Start at boot" became "Open in the tray at login" (the API runs regardless). @vhidvz
- Updates restart the background API on the new version instead of refusing while posd runs (`posdRunning` is gone). @vhidvz
- `--install` sets up the service, `--uninstall` removes it; a rollback to before 1.2 removes it too. @vhidvz
- posd publishes where it serves (`serving.json`): after a failed port change the console still reaches it. @vhidvz

### Fixed

- posd finishes the payment in flight on SIGTERM (300 s cap) instead of dropping it; `/api/v1/health` names the process. @vhidvz

## [1.1.1] - 2026-10-17

### Fixed

- Guide: the posd systemd box is now a command that writes the unit; `enable` no longer fails "does not exist". @vhidvz

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
