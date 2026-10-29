#!/bin/sh
# aca.bark: push one notification per event. The address is the url setting (ACA_SETTING_URL); nothing is pushed while someone
# is attached to the session; for workers that were sent out, see the workers setting.
url="$ACA_SETTING_URL"
[ -n "$url" ] || { echo "aca.bark: no url set (settings.url in plugins.toml)"; exit 0; }
for c in curl jq; do
  command -v "$c" >/dev/null 2>&1 || { echo "aca.bark: needs $c"; exit 0; }
done
push() {
  curl -sS --max-time 5 -X POST "$url" -H 'Content-Type: application/json' \
    -d "$(jq -n --arg t "$1" --arg b "$2" --arg g "${ACA_SETTING_GROUP:-aca}" '{title: $t, body: $b, group: $g}')" >/dev/null
}
if [ "$1" = "test" ]; then
  push "aca" "Bark works" && echo "pushed"
  exit
fi
row=$("$ACA_BIN" show "$ACA_SESSION_ID" --json) || exit 0
[ "$(printf '%s' "$row" | jq -r '.attached // 0')" = "0" ] || exit 0
if [ "$ACA_SETTING_WORKERS" != "true" ] && [ -n "$(printf '%s' "$row" | jq -r '.parent // empty')" ]; then
  exit 0
fi
title=$(printf '%s' "$row" | jq -r .title)
ev="$ACA_PLUGIN_EVENT_JSON"
case "$ACA_PLUGIN_EVENT" in
  turn.done) push "✓ $title" "$(printf '%s' "$ev" | jq -r '(.last // "") | .[0:200]')" ;;
  agent.waiting) push "? $title" "$(printf '%s' "$ev" | jq -r '.ask // ""')" ;;
  turn.failed) push "✕ $title" "$(printf '%s' "$ev" | jq -r '(.error // "") | .[0:200]')" ;;
esac
