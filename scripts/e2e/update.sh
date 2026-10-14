#!/usr/bin/env bash
# End-to-end proof of the update path (spec §8), isolated from the machine's real install.
#
#   OLD=<1.0.0 e2e AppImage> NEW=<1.0.1-test e2e AppImage> OUT=<evidence dir> \
#   [LEGACY=<pre-1.0 AppImage>] [LEGACY_POSD=<pre-1.0 posd, named exactly `posd`>] scripts/e2e/update.sh
#
# e2e AppImages are built with `--features e2e`, the only build in which LEDGER_POS_ADMIN_PATH
# moves the administrator file. Every scenario runs in its own temp HOME and XDG dirs, on a
# private session bus, on a port checked free. The real install is fingerprinted before and
# after; any difference fails the run.
#
# The update *screen* is driven the same way: an e2e build honours LEDGER_POS_E2E_CLICK=<css
# selector> in update mode, clicking that element through the webview ~4 s after the window
# opens (e.g. "button.btn-primary" for Update and restart) — injected pointer input does not
# reach windows on every desktop.
set -euo pipefail

say() { printf '\n== %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
# json '<expr over v>' — evaluate a JS expression over stdin's JSON.
# Prints nothing for null/undefined, so "API down" reads as an empty string.
json() { node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{let v=null;try{v=s?JSON.parse(s):null}catch{}const r=Function("v","return "+process.argv[1])(v);console.log(r==null?"":typeof r==="string"?r:JSON.stringify(r))})' "$1"; }

real_fingerprint() {
  for f in ~/.local/bin/ledger-pos.AppImage ~/.local/bin/posd \
           ~/.local/share/applications/ledger-pos.desktop ~/.config/ledger-pos/settings.json \
           /etc/ledger-pos/admin.json; do
    if [ -e "$f" ]; then sha256sum "$f"; else echo "absent $f"; fi
  done
  echo "real api: $(curl -s -m 5 http://127.0.0.1:4373/api/v1/health | json 'v&&v.version' || echo down)"
  echo "real listener: $(ss -ltnpH 'sport = :4373' | grep -o 'pid=[0-9]*' | head -1)"
}

if [ -z "${E2E_INNER:-}" ]; then
  : "${OLD:?}" "${NEW:?}" "${OUT:?}"
  mkdir -p "$OUT"
  real_fingerprint > "$OUT/real-before.txt"
  set +e
  E2E_INNER=1 dbus-run-session -- "$0" "$@" 2>&1 | tee "$OUT/run.log"
  status=${PIPESTATUS[0]}
  set -e
  real_fingerprint > "$OUT/real-after.txt"
  if diff -u "$OUT/real-before.txt" "$OUT/real-after.txt"; then
    echo "REAL INSTALL UNTOUCHED"
  else
    echo "REAL INSTALL CHANGED"; exit 1
  fi
  exit "$status"
fi

ROOT=$(mktemp -d "${TMPDIR:-/tmp}/ledger-e2e.XXXXXX")
PORT=

free_port() {
  for p in $(seq 47300 47399); do
    ss -ltnH "sport = :$p" | grep -q . || { echo "$p"; return; }
  done
  fail "no free port in 47300-47399"
}

fresh_home() {
  local h="$ROOT/home-$1"
  mkdir -p "$h"
  export HOME="$h" XDG_CONFIG_HOME="$h/.config" XDG_DATA_HOME="$h/.local/share" XDG_CACHE_HOME="$h/.cache"
  export LEDGER_POS_ADMIN_PATH="$h/admin.json"
  PORT=$(free_port)
  mkdir -p "$XDG_CONFIG_HOME/ledger-pos"
  # A pre-1.0 settings file: no schemaVersion.
  cat > "$XDG_CONFIG_HOME/ledger-pos/settings.json" <<JSON
{ "server": { "host": "127.0.0.1", "port": $PORT, "corsAllowedOrigins": [], "corsAllowAll": false },
  "app": { "minimizeToTrayOnClose": true, "startAtBoot": false, "startMinimized": true, "installPromptDismissed": true },
  "providers": { "active": "sandbox", "configs": { "sandbox": { "latencyMs": 0, "cardPresentDelayMs": 8000 } } } }
JSON
}

api() { curl -sS -m 60 "http://127.0.0.1:$PORT$1" "${@:2}"; }
version_on_port() { api /api/v1/health 2>/dev/null | json 'v&&v.version' 2>/dev/null || true; }
wait_version() {
  for _ in $(seq 1 120); do [ "$(version_on_port)" = "$1" ] && return 0; sleep 0.5; done
  fail "API on :$PORT never reported $1 (last: $(version_on_port))"
}
wait_down() {
  for _ in $(seq 1 120); do [ -z "$(version_on_port)" ] && return 0; sleep 0.5; done
  fail "API on :$PORT still up"
}
start_installed() { setsid "$HOME/.local/bin/ledger-pos.AppImage" --background >>"$ROOT/app.log" 2>&1 < /dev/null & }
stop_installed() { "$HOME/.local/bin/ledger-pos.AppImage" --quit-for-update >>"$ROOT/app.log" 2>&1 || true; wait_down; }
key() { grep -o 'X-LedgerPOS-Version=.*' "$XDG_DATA_HOME/applications/ledger-pos.desktop" | cut -d= -f2; }
cfg() { cat "$XDG_CONFIG_HOME/ledger-pos/$1"; }
port_pid() { ss -ltnpH "sport = :$PORT" | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2; }

scenario_running_sale() {
  say "B: 1.0.0 running with a sale in flight → 1.0.1-test"
  fresh_home b
  "$OLD" --install
  start_installed
  wait_version 1.0.0
  api /api/v1/providers/sandbox/functions/purchase/invoke -X POST -H 'content-type: application/json' \
    -d '{"mainAmount":1000}' -o "$ROOT/b-sale.json" -w '%{http_code}' > "$ROOT/b-sale.code" &
  local sale=$!
  sleep 1
  "$NEW" --update | tee "$ROOT/b-update.log"
  wait "$sale"
  [ "$(cat "$ROOT/b-sale.code")" = 200 ] || fail "the in-flight sale did not complete: $(cat "$ROOT/b-sale.code")"
  wait_version 1.0.1-test
  [ "$(key)" = 1.0.1-test ] || fail "menu entry names $(key)"
  [ -n "$(json 'v.gen' < "$XDG_DATA_HOME/ledger-pos/prev.json")" ] || fail "no rollback generation"
  [ "$(cfg settings.json | json 'v.schemaVersion')" = 1 ] || fail "settings not migrated"
  [ -e "$XDG_CONFIG_HOME/ledger-pos/settings.json.v0.bak" ] || fail "no settings backup"
  # 1.0.0's own first start migrated the pre-1.0 settings and left a notice from "before 1.0";
  # unacknowledged, it keeps that earliest `from` (spec §4) and now runs to 1.0.1-test.
  [ "$(cfg state.json | json 'v.app.lastRunVersion')" = 1.0.1-test ] || fail "lastRunVersion: $(cfg state.json)"
  [ "$(cfg state.json | json 'v.app.pendingNotice.to')" = 1.0.1-test ] || fail "notice: $(cfg state.json)"
  say "B: still running 30 s after the updater exited (ruling R4)"
  sleep 30
  wait_version 1.0.1-test
  stop_installed
  echo "B PASS"
}

scenario_legacy() {
  say "A: pre-1.0 install running in the tray → 1.0.1-test"
  fresh_home a
  "$LEGACY" --install
  [ -z "$(key || true)" ] || fail "the legacy install already has a version key"
  start_installed
  for _ in $(seq 1 120); do [ -n "$(version_on_port)" ] && break; sleep 0.5; done
  [ -n "$(version_on_port)" ] || fail "legacy app did not start"
  "$NEW" --update > "$ROOT/a-update.log" 2>&1 &
  local upd=$!
  sleep 13
  grep -q 'tray' "$ROOT/a-update.log" || fail "no tray hint after 10 s: $(cat "$ROOT/a-update.log")"
  # The user chooses Quit from the tray: the legacy process owning our port exits.
  kill -TERM "$(port_pid)"
  wait "$upd" || fail "update failed: $(cat "$ROOT/a-update.log")"
  wait_version 1.0.1-test
  [ "$(cfg settings.json | json 'v.schemaVersion')" = 1 ] || fail "settings not migrated"
  [ -e "$XDG_CONFIG_HOME/ledger-pos/settings.json.v0.bak" ] || fail "no settings backup"
  [ "$(cfg state.json | json 'v.app.pendingNotice.from === null')" = true ] || fail "notice: $(cfg state.json)"
  stop_installed
  echo "A PASS"
}

scenario_legacy_posd() {
  say "C: a pre-1.0 posd running on its own blocks the update until stopped"
  fresh_home c
  "$OLD" --install
  setsid "$LEGACY_POSD" >>"$ROOT/c-posd.log" 2>&1 < /dev/null &
  local posd=$!
  for _ in $(seq 1 60); do [ -n "$(version_on_port)" ] && break; sleep 0.5; done
  set +e; "$NEW" --update > "$ROOT/c-update-1.log" 2>&1; local code=$?; set -e
  [ "$code" = 4 ] || fail "expected exit 4 while posd runs, got $code"
  grep -q 'posd is running' "$ROOT/c-update-1.log" || fail "no posd message"
  [ "$(key)" = 1.0.0 ] || fail "the install changed while posd ran"
  kill -TERM "$posd"; wait_down
  "$NEW" --update | tee "$ROOT/c-update-2.log"
  wait_version 1.0.1-test
  stop_installed
  local before; before=$(cfg state.json | json 'v.migrations.length')
  setsid "$HOME/.local/bin/posd" >>"$ROOT/c-posd.log" 2>&1 < /dev/null &
  local newposd=$!
  wait_version 1.0.1-test
  kill -TERM "$newposd"; wait_down
  [ "$(cfg state.json | json 'v.migrations.length')" = "$before" ] || fail "the new posd migrated again"
  [ "$(cfg state.json | json 'v.app.lastRunVersion')" = 1.0.1-test ] || fail "posd touched app"
  echo "C PASS"
}

scenario_downgrade() {
  say "D: an older file over a newer install; newer data stays read-only; rollback"
  fresh_home d
  "$NEW" --install
  set +e; "$OLD" --install > "$ROOT/d-1.log" 2>&1; local code=$?; set -e
  [ "$code" = 3 ] || fail "expected a refusal (3), got $code"
  "$OLD" --install --allow-downgrade
  [ "$(key)" = 1.0.0 ] || fail "downgrade did not install 1.0.0"
  node -e 'const f=process.argv[1];const fs=require("fs");const v=JSON.parse(fs.readFileSync(f));v.schemaVersion=9;fs.writeFileSync(f,JSON.stringify(v))' \
    "$XDG_CONFIG_HOME/ledger-pos/settings.json"
  local sum; sum=$(sha256sum "$XDG_CONFIG_HOME/ledger-pos/settings.json")
  start_installed
  wait_version 1.0.0
  local code_put
  code_put=$(api /api/v1/settings | api /api/v1/settings -X PUT -H 'content-type: application/json' --data-binary @- -o "$ROOT/d-put.json" -w '%{http_code}')
  [ "$code_put" = 409 ] || fail "PUT settings: $code_put $(cat "$ROOT/d-put.json")"
  [ "$(api /api/v1/system | json 'v.data.settings')" = readOnlyNewer ] || fail "system does not report readOnlyNewer"
  stop_installed
  [ "$(sha256sum "$XDG_CONFIG_HOME/ledger-pos/settings.json")" = "$sum" ] || fail "a newer settings file was written"
  "$HOME/.local/bin/ledger-pos.AppImage" --rollback
  [ "$(key)" = 1.0.1-test ] || fail "rollback named $(key)"
  [ "$("$HOME/.local/bin/ledger-pos.AppImage" --version)" = "Ledger POS 1.0.1-test" ] || fail "binary and key disagree"
  echo "D PASS"
}

scenario_running_sale
scenario_downgrade
if [ -n "${LEGACY:-}" ]; then scenario_legacy; else echo "A SKIPPED (no LEGACY)"; fi
if [ -n "${LEGACY_POSD:-}" ]; then scenario_legacy_posd; else echo "C SKIPPED (no LEGACY_POSD)"; fi
say "ALL SCENARIOS DONE"
