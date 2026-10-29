#!/bin/sh
# aca.discord: post one message to the channel per event. The address is the url setting (ACA_SETTING_URL); nothing is posted
# while someone is attached to the session; for workers that were sent out, see the workers setting.
url="$ACA_SETTING_URL"
[ -n "$url" ] || { echo "aca.discord: no url set (aca plugin setting aca.discord url <webhook>)"; exit 0; }
for c in curl jq; do
  command -v "$c" >/dev/null 2>&1 || { echo "aca.discord: needs $c"; exit 0; }
done
# post TITLE BODY [@WHO]: a Discord message holds at most 2000 characters, so the body is cut short; only the person in the
# settings may be @-mentioned
post() {
  body=$(jq -n --arg t "$1" --arg b "$2" --arg m "$3" --arg u "${ACA_SETTING_USERNAME:-aca}" '{
    username: $u,
    content: ((if $m != "" then "<@\($m)> " else "" end) + "**\($t)**" + (if $b != "" then "\n" + ($b | .[0:1500]) else "" end)),
    allowed_mentions: { users: (if $m != "" then [$m] else [] end) }
  }')
  curl -sS --max-time 10 -X POST "$url" -H 'Content-Type: application/json' -d "$body" >/dev/null
}
if [ "$1" = "test" ]; then
  post "aca" "Discord works" && echo "posted"
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
  turn.done) post "✓ $title" "$(printf '%s' "$ev" | jq -r '(.last // "") | .[0:300]')" ;;
  agent.waiting) post "? $title" "$(printf '%s' "$ev" | jq -r '.ask // ""')" "$ACA_SETTING_MENTION" ;;
  turn.failed) post "✕ $title" "$(printf '%s' "$ev" | jq -r '(.error // "") | .[0:300]')" ;;
esac
