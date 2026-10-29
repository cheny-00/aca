#!/bin/sh
# The tools of aca.core: one aca command per tool. Arguments are in ACA_ARG_<NAME>; the calling session is ACA_SESSION_ID, and
# its directory ACA_SESSION_DIR.
# Exit codes are passed back as they are (a nonzero one shows up as an error on the agent side); wait is the exception: its exit
# code is the result, so it is printed and the script itself exits 0.
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
    # shellcheck disable=SC2086 # ids is several, separated by spaces
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
  wakeup)
    if [ -z "$ACA_SESSION_ID" ]; then
      echo "aca.core: not inside an aca session" >&2
      exit 2
    fi
    exec "$a" msg --in "$ACA_ARG_AFTER" "$ACA_SESSION_ID" "$ACA_ARG_TEXT"
    ;;
  *)
    echo "aca.core: no tool $1" >&2
    exit 2
    ;;
esac
