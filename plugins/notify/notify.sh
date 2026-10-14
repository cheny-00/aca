#!/bin/sh
# aca.notify:事件来了在这台机器上弹一条桌面通知。怎么弹看设置 command(ACA_SETTING_COMMAND);
# 你正连着这个会话就不弹(设置 attached);派出去的 worker 看设置 workers。
command -v jq >/dev/null 2>&1 || { echo "aca.notify: needs jq"; exit 0; }
# show 标题 正文
show() {
  # 你换的命令发不出去(退出码不是 0,比如 kitty 没开)就照空着的办,不漏通知
  if [ -n "${ACA_SETTING_COMMAND:-}" ] && sh -c "$ACA_SETTING_COMMAND \"\$@\"" aca.notify "$1" "$2"; then
    return 0
  fi
  if command -v osascript >/dev/null 2>&1; then
    # 标题、正文当参数交进去:不用转义引号
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
# 正文:去掉代码块的围栏、反引号、加粗、标题的 #,空白并成一个,截到 200 字
text() {
  printf '%s' "$ACA_PLUGIN_EVENT_JSON" | jq -r --arg k "$1" \
    '(.[$k] // "") | gsub("```[A-Za-z0-9_-]*"; "") | gsub("`|\\*\\*"; "") | gsub("(^|\n)\\s{0,3}#{1,6}\\s+"; "\n") | gsub("\\s+"; " ") | sub("^ "; "") | .[0:200]'
}
case "$ACA_PLUGIN_EVENT" in
  turn.done) show "✓ $title" "$(text last)" ;;
  agent.waiting) show "? $title" "$(text ask)" ;;
  turn.failed) show "✕ $title" "$(text error)" ;;
esac
