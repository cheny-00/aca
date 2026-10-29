#!/bin/sh
# A worker that was sent out (a session with a parent) failed a turn, transiently (rate limit, overload, dropped stream;
# turn.failed with kind = transient): wait a while (the delay setting in seconds × which attempt this is, default 30), and if it
# is still stuck on the error, say one line for it (the say setting, default continue). At most max_continue times per worker
# (default 5), reset after a turn that finishes normally. Account problems, and whatever a retry will not fix, are left alone
# for whoever sent it out (it sees them on aca wait).
# Sessions you started yourself (no parent) are left alone. The if in the manifest already lets only these in; checked again below.
# Settings are changed in plugins.toml; the environment variables ACA_DELEGATE_MAX_CONTINUE / ACA_DELEGATE_DELAY override them
# (for tests).

id="$ACA_SESSION_ID"
[ -n "$id" ] || exit 0
count="$ACA_PLUGIN_STATE_DIR/continues-$id"

if [ "$ACA_PLUGIN_EVENT" = "turn.done" ]; then
  rm -f "$count"
  exit 0
fi

[ "$(printf '%s' "$ACA_PLUGIN_EVENT_JSON" | jq -r '.kind // ""')" = "transient" ] || exit 0
[ -n "$ACA_SESSION_PARENT" ] || exit 0

n=$(cat "$count" 2>/dev/null || echo 0)
max="${ACA_DELEGATE_MAX_CONTINUE:-${ACA_SETTING_MAX_CONTINUE:-5}}"
if [ "$n" -ge "$max" ]; then
  echo "$id:已代为发送 $n 次,达到上限,不再发送"
  exit 0
fi
n=$((n + 1))
mkdir -p "$ACA_PLUGIN_STATE_DIR"
echo "$n" > "$count"

sleep $((${ACA_DELEGATE_DELAY:-${ACA_SETTING_DELAY:-30}} * n))
# Someone took over in the meantime (the one that sent it out did a send, or you typed in it): do not butt in again
state=$("$ACA_BIN" show "$id" --json | jq -r '.state // ""')
if [ "$state" != "error" ]; then
  echo "$id:当前状态为 $state,无需再发送"
  exit 0
fi
say="${ACA_SETTING_SAY:-continue}"
echo "$id:第 $n 次代为发送「${say}」"
"$ACA_BIN" send "$id" -m "$say"
