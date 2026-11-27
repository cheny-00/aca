#!/bin/sh
# With token and channel set the bot posts (discord.py, which keeps the thread records pull.sh reads); otherwise the webhook.
if [ -n "$ACA_SETTING_TOKEN" ] && [ -n "$ACA_SETTING_CHANNEL" ]; then
  command -v python3 >/dev/null 2>&1 || { echo "aca.discord: needs python3"; exit 0; }
  exec python3 "$(dirname "$0")/discord.py" post "$@"
fi
# Webhook: Discord-originated turns return their full reply through the configured webhook. Other events use the ordinary
# preview; the session lifetime events (for the bot's threads) are nothing to it.
case "$ACA_PLUGIN_EVENT" in session.*) exit 0 ;; esac
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
ev="$ACA_PLUGIN_EVENT_JSON"
if [ "$(printf '%s' "$ev" | jq -r '.via // empty')" = Discord ]; then
  # Split after adding the mention so it counts toward the first message's limit.
  mention=
  case "$ACA_PLUGIN_EVENT" in
    turn.done) field=last ;;
    agent.waiting) field=ask; mention=$ACA_SETTING_MENTION ;;
    turn.failed) field=error ;;
    *) exit 0 ;;
  esac
  payloads=$(printf '%s' "$ev" | jq -c --arg field "$field" --arg u "${ACA_SETTING_USERNAME:-aca}" --arg m "$mention" '
    ((if $m == "" then "" else "<@\($m)> " end) + (.[$field] // "")[:4000]) as $text |
    range(0; $text | length; 2000) as $i |
    {username: $u, content: $text[$i:$i+2000],
     allowed_mentions: (if $i == 0 and $m != "" then {parse: [], users: [$m]} else {parse: []} end)}') || exit 1
  [ -n "$payloads" ] || exit 0
  printf '%s\n' "$payloads" | while IFS= read -r body; do
    curl -fsS --max-time 10 -X POST "$url" -H 'Content-Type: application/json' -d "$body" >/dev/null || exit 1
  done
  exit $?
fi
row=$("$ACA_BIN" show "$ACA_SESSION_ID" --json) || exit 0
[ "$(printf '%s' "$row" | jq -r '.attached // 0')" = "0" ] || exit 0
if [ "$ACA_SETTING_WORKERS" != "true" ] && [ -n "$(printf '%s' "$row" | jq -r '.parent // empty')" ]; then
  exit 0
fi
title=$(printf '%s' "$row" | jq -r .title)
case "$ACA_PLUGIN_EVENT" in
  turn.done) post "✓ $title" "$(printf '%s' "$ev" | jq -r '(.last // "") | .[0:300]')" ;;
  agent.waiting) post "? $title" "$(printf '%s' "$ev" | jq -r '.ask // ""')" "$ACA_SETTING_MENTION" ;;
  turn.failed) post "✕ $title" "$(printf '%s' "$ev" | jq -r '(.error // "") | .[0:300]')" ;;
esac
