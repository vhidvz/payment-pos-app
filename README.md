# Ledger POS

Turns a payment terminal into a **local REST API**. Your till, web shop or script
asks for a payment, the customer taps their card, you get the receipt back as
JSON. A desktop console comes with it for setup and monitoring, and keeps serving
from the system tray once you close the window.

Drives the **Saman SSP1126**. A built-in **sandbox** terminal simulates the same
operations so you can build your integration without hardware.

![The Ledger POS console](screenshot.png)

## Install

No prebuilt downloads yet — you build it once, then install what that produces.

### 1. Prerequisites

Everywhere: **Node 22**, **pnpm 10** (`corepack enable pnpm`) and **Rust**
([rustup.rs](https://rustup.rs)). Then, per system — the full list is in the
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/):

```sh
# Arch / Manjaro
sudo pacman -S --needed base-devel openssl webkit2gtk-4.1 gtk3 libsoup3 \
                        librsvg libappindicator-gtk3

# Debian / Ubuntu
sudo apt install build-essential curl wget file libssl-dev \
     libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev \
     libayatana-appindicator3-dev
```

**Windows**: Microsoft C++ Build Tools, and the WebView2 runtime (already present
on Windows 11). Build on Windows itself — Tauri cannot cross-compile there from
Linux.

On Linux, `libappindicator` is what draws the tray icon; without it, closing the
window leaves you no way back to the app.

### 2. Build

```sh
git clone https://github.com/vhidvz/payment-pos-app.git
cd payment-pos-app
pnpm install
pnpm tauri build
```

The first build compiles Rust from scratch — give it a few minutes. Installers
land in `src-tauri/target/release/bundle/`.

### 3. Install and start

**Arch, Manjaro, and every other Linux — use the AppImage.** Make it executable
and open it. The first time it runs it offers to add itself to your applications
menu; accept, and you are done. Nothing needs a password.

```sh
cd src-tauri/target/release/bundle/appimage
chmod +x "Ledger POS_0.1.0_amd64.AppImage"
./"Ledger POS_0.1.0_amd64.AppImage"
```

It copies itself to `~/.local/bin/ledger-pos.AppImage`, writes the menu entry and
icons, and puts `posd` beside it — all in your home directory. From then on
launch **Ledger POS** from the menu; the offer is never made again.

The same thing without the window, for setting up several machines:

```sh
./"Ledger POS_0.1.0_amd64.AppImage" --install         # --uninstall, --install-status
```

`--uninstall` takes back exactly those files and leaves your settings alone.

> **One catch:** `sudo` does not search `~/.local/bin`, so the administrator
> commands below need the full path after an AppImage install —
> `sudo ~/.local/bin/posd admin status`. Package installs put `posd` in
> `/usr/bin`, where plain `sudo posd …` works.

**Distribution packages**, from `src-tauri/target/release/bundle/`:

| System | Command |
| --- | --- |
| Debian / Ubuntu | `sudo apt install "./deb/Ledger POS_0.1.0_amd64.deb"` |
| Fedora / RHEL | `sudo rpm -i "./rpm/Ledger POS-0.1.0-1.x86_64.rpm"` |
| Windows | run `msi/Ledger POS_0.1.0_x64_en-US.msi` |

These put `payment-pos-app` and `posd` in `/usr/bin` and add **Ledger POS** to
the application menu — the package manager does the whole job, so there is no
wizard and nothing further to do. Arch and Manjaro take neither format; use the
AppImage above.

## Setup

The app starts on the sandbox, so everything works before hardware is involved.
For a real terminal: **Providers → Saman SSP1126**, set **host** (its IP) and
**port** (`1197`), *Save*, then *Make active*. Serial instead? Set **transport**
to `serial` and give the device **path**.

The sidebar shows a live terminal light. Green means it is answering.

> **After every power-on, put a card in the terminal once — you can cancel it.**
> An SSP1126 does not open its link to the PC when it boots; it opens it the
> first time a card wakes its payment application, and keeps it open afterwards.
> Until then it refuses every connection and the app reports
> `terminal_unreachable`. Once per power-on, not once per sale.

## API

Base URL `http://127.0.0.1:4373`. Interactive docs at **`/docs`**.

```sh
curl -X PUT localhost:4373/api/v1/providers/saman-ssp1126/config \
  -H 'content-type: application/json' \
  -d '{"transport":"tcp","host":"192.168.1.198","port":1197}'

curl -X POST localhost:4373/api/v1/providers/saman-ssp1126/functions/purchase/invoke \
  -H 'content-type: application/json' \
  -d '{"mainAmount":10000,"referenceData":"ORDER-1001"}'
```

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/v1/health` | Is it alive |
| GET | `/api/v1/system` | Version, active terminal, server address, settings file |
| GET/PUT | `/api/v1/settings` | Read / replace all settings |
| GET | `/api/v1/activity` | Recent operations |
| GET | `/api/v1/providers` | Installed terminal types |
| GET/PUT | `/api/v1/providers/active` | Read / switch the active terminal |
| GET | `/api/v1/providers/{id}` | Everything about one terminal type |
| GET/PUT | `/api/v1/providers/{id}/config` | Read / change its configuration |
| GET | `/api/v1/providers/{id}/link` | Is the terminal reachable right now |
| GET | `/api/v1/providers/{id}/functions[/{fn}]` | Operation catalogue / one operation |
| POST | `/api/v1/providers/{id}/functions/{fn}/invoke` | Run an operation |
| POST | `/api/v1/providers/active/functions/{fn}/invoke` | Run it on the active terminal |
| GET | `/api/v1/auth/status` | Is a password set, are you locked |
| POST | `/api/v1/auth/unlock` · `/lock` | Get / give up a token |

**Use generous timeouts** — card operations block until the cardholder acts, up
to about two minutes. Only one transaction runs per terminal at a time.

Failures are `{ "code": "...", "error": "..." }`:

`400 invalid_params` · `401 locked` `invalid_password` · `404 unknown_provider`
`unknown_function` · `409 busy` · `412 not_configured` `no_password_set` ·
`502 execution_failed` (terminal reached, operation failed) ·
`503 terminal_unreachable` (terminal not accepting connections)

## Settings

All in `~/.config/ledger-pos/settings.json` (`%APPDATA%\\ledger-pos\\settings.json` on
Windows) — editable in the app, over the API,
or by hand. Changing the API host/port rebinds immediately; if the new address
cannot be used it falls back to the last working one and reports why in
`/api/v1/system`, so you cannot lock yourself out.

Closing the window keeps the API serving from the tray. *Settings → Start at
boot* launches it hidden at login. Or run `posd` with no desktop at all — same
API, same port, so run one or the other.

## Locking the app

Optional and off by default. Only someone with `sudo` can set or remove the
password; anyone who knows it can unlock the console; everyone else can look but
not change anything.

```sh
sudo posd admin set-password      # asks twice, shows nothing as you type
sudo posd admin clear-password    # forgot it? this is the way back
sudo posd admin auto-lock 15      # idle minutes before re-locking; 0 never
sudo posd admin status
```

After an AppImage install, give the full path — `sudo` does not look in
`~/.local/bin`: `sudo ~/.local/bin/posd admin set-password`.

The hashed password goes in an administrator-only file — `/etc/ledger-pos/admin.json`
on Linux, `%ProgramData%\\ledger-pos\\admin.json` on Windows (run the commands from
an Administrator prompt there, without `sudo`). Deliberately not in
`settings.json`, which belongs to whoever runs the app and could otherwise be
edited to strip the password out.

| | While locked |
| --- | --- |
| Changing settings, terminal configuration or the active terminal, in the app **or** over the API | blocked |
| Running operations by hand in the Operations screen | blocked |
| `POST …/invoke` and every `GET` | **still works** |

So the till keeps trading while nobody at the counter can repoint the terminal.
Unlock with the padlock in the sidebar, or `POST /api/v1/auth/unlock` then send
`Authorization: Bearer <token>`. It re-locks on idle and on restart, and wrong
passwords are answered more slowly after the third try.

**This is an operational control, not a security boundary.** `/invoke` stays open
on purpose, so anyone with a terminal window on this machine can still drive the
payment terminal with `curl` — and `settings.json` belongs to the account the app
runs as, so a shell as *that* account can edit the terminal address regardless.
Separate accounts close those doors, `sudo` does not: run the daemon as its own
user (`User=ledgerpos`) and then `chgrp ledgerpos` + `chmod 640` the password
file.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| Terminal light red, `terminal_unreachable` | Put a card in the terminal once and cancel. Then check its IP and port. |
| Connection test fails but payments work | Some terminals ignore the test message; the app checks a second way, so a failure here is real. |
| `409 busy` | One transaction at a time — wait for the previous one. |
| A transaction occasionally takes ~20s | The terminal sometimes ignores the opening message; the app re-sends it automatically. Raise **firstAckAttempts** on the provider page if it still gives up. |
| API did not start | Port taken, or the app and `posd` are both running. See `/api/v1/system`. |
| Forgot the password | `sudo posd admin clear-password` on the machine. |

## Security notes

- The API listens on `127.0.0.1` and does not authenticate payments even with the
  lock on — anything on this machine can drive the terminal. Change `server.host`
  only if you accept that other machines can too.
- `corsAllowAll` would let any web page you visit call the API from your browser.
- The link to the terminal is checked for corruption but **not encrypted**. Keep
  it on a network you trust.

## License

Copyright (c) 2026 Vahid V. Released under the MIT License — see [LICENSE](./LICENSE).
