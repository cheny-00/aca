#!/bin/sh
# aca.core 的工具:一个工具一条 aca 命令。参数在 ACA_ARG_<名字>,调用的会话是 ACA_SESSION_ID、它的目录是 ACA_SESSION_DIR。
# 退出码原样交回(不是 0 的,agent 那边看到的是出错);wait 例外:它的退出码是结果,打出来、自己退 0。
a="$ACA_BIN"
case "$1" in
  sessions)
    if [ -n "$ACA_ARG_ID" ]; then
      exec "$a" show "$ACA_ARG_ID" --json
    fi
    exec "$a" ls --json --no-screen
    ;;
  dispatch)
    set -- new "$ACA_ARG_TOOL" "${ACA_ARG_DIR:-${ACA_SESSION_DIR:-.}}" --say "$ACA_ARG_TASK"
    [ -n "$ACA_ARG_BRANCH" ] && set -- "$@" -w "$ACA_ARG_BRANCH"
    [ -n "$ACA_ARG_TITLE" ] && set -- "$@" -t "$ACA_ARG_TITLE"
    exec "$a" "$@"
    ;;
  message)
    exec "$a" msg "$ACA_ARG_TO" "$ACA_ARG_TEXT"
    ;;
  wait)
    # shellcheck disable=SC2086 # ids 是空格隔开的几个
    if [ "$ACA_ARG_ANY" = "true" ]; then
      "$a" wait $ACA_ARG_IDS --any --timeout 240
    else
      "$a" wait $ACA_ARG_IDS --timeout 240
    fi
    echo "exit code $?"
    ;;
  answer)
    if [ -n "$ACA_ARG_ANSWER" ]; then
      exec "$a" answer "$ACA_ARG_ID" "$ACA_ARG_ANSWER"
    fi
    exec "$a" answer "$ACA_ARG_ID"
    ;;
  interrupt)
    exec "$a" interrupt "$ACA_ARG_ID"
    ;;
  *)
    echo "aca.core: no tool $1" >&2
    exit 2
    ;;
esac
