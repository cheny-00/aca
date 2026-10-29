#!/bin/sh
# aca.notify: show a desktop notification on this machine per event. How it is shown: the command setting (ACA_SETTING_COMMAND);
# nothing is shown while you are attached to the session (the attached setting); for workers that were sent out, see the workers setting.
command -v jq >/dev/null 2>&1 || { echo "aca.notify: needs jq"; exit 0; }
# show TITLE BODY
show() {
  # If your replacement command fails (exit code not 0, e.g. kitty is not open), act as if it were empty, so no notification is lost
  if [ -n "${ACA_SETTING_COMMAND:-}" ] && sh -c "$ACA_SETTING_COMMAND \"\$@\"" aca.notify "$1" "$2"; then
    return 0
  fi
  if command -v osascript >/dev/null 2>&1; then
    # Title and body are passed in as arguments: no quote escaping needed
    osascript -e 'on run argv' -e 'display notification (item 2 of argv) with title (item 1 of argv)' -e 'end run' "$1" "$2" >/dev/null
  elif command -v notify-send >/dev/null 2>&1; then
    notify-send -- "$1" "$2"
  else
    echo "aca.notify: no osascript or notify-send here (aca plugin setting aca.notify command <program>)"
    return 1
  fi
}
if [ "$1" = "test" ]; then
  show "aca" "Notify works" && echo "shown"
  exit
fi
[ "$ACA_SETTING_WORKERS" = "true" ] || [ -z "$ACA_SESSION_PARENT" ] || exit 0
row=$("$ACA_BIN" show "$ACA_SESSION_ID" --json) || exit 0
[ "$ACA_SETTING_ATTACHED" = "true" ] || [ "$(printf '%s' "$row" | jq -r '.attached // 0')" = "0" ] || exit 0
title=$(printf '%s' "$row" | jq -r .title)
# Body: drop code fences, backticks, bold and heading #s, squeeze whitespace to one space, cut to 200 characters
text() {
  printf '%s' "$ACA_PLUGIN_EVENT_JSON" | jq -r --arg k "$1" \
    '(.[$k] // "") | gsub("```[A-Za-z0-9_-]*"; "") | gsub("`|\\*\\*"; "") | gsub("(^|\n)\\s{0,3}#{1,6}\\s+"; "\n") | gsub("\\s+"; " ") | sub("^ "; "") | .[0:200]'
}
case "$ACA_PLUGIN_EVENT" in
  turn.done) show "✓ $title" "$(text last)" ;;
  agent.waiting) show "? $title" "$(text ask)" ;;
  turn.failed) show "✕ $title" "$(text error)" ;;
esac
