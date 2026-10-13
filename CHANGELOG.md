# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Optional webhook events in Standard Webhooks format: lock, password, settings, app start/stop. `posd admin events`. @vhidvz
- A change to the event subscriptions is announced to the old endpoints too; undelivered events retry for 72 hours. @vhidvz
- Optional signed receipts: an `attestation` JWS (HS256) on each invoke of the active terminal; `posd admin receipt-key`. @vhidvz
- Events and receipts carry the device posture: password set, unlocked now, and a fingerprint of the payment settings. @vhidvz
- `GET /api/v1/events` shows subscriptions (scheme and host only), signing and delivery state, never a secret. @vhidvz
- Data files carry a schema version and migrate on start, keeping a `<file>.v<N>.bak` copy of the original. @vhidvz
- Settings written by a newer version are never overwritten: changes are refused with `409 settings_read_only`. @vhidvz
- A newer version's administrator file keeps the console locked; `posd admin` refuses to change it. @vhidvz

### Fixed

- `posd admin` no longer resets the administrator file to 0644 on every write, undoing an administrator's `chmod 640`. @vhidvz
- `posd admin` refuses to write over an administrator file it cannot parse, instead of erasing the password hash. @vhidvz
- One malformed webhook or key entry no longer makes the whole administrator file read as "no password set". @vhidvz
- The administrator file's temporary copy is created with its final permissions, never the default umask. @vhidvz
