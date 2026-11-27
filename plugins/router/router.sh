#!/bin/sh
set -ef
a=$ACA_BIN
group=$ACA_ROUTER_GROUP
watch=
case "${ACA_ARG_WATCH-}" in
  true) watch=--watch ;;
  false) watch=--no-watch ;;
esac
case "$1" in
  sessions) exec "$a" ls "-g=$group" ;;
  look)
    "$a" show "$ACA_ARG_ID" --summary
    exec "$a" output "$ACA_ARG_ID" --tail 2000
    ;;
  tickets) exec "$a" task ls "-g=$group" --recent ;;
  ticket)
    set -- task add "-g=$group" "-t=$ACA_ARG_TITLE" "--criteria=$ACA_ARG_CRITERIA"
    [ -z "$ACA_ARG_TO" ] || set -- "$@" "--to=$ACA_ARG_TO"
    [ -z "$ACA_ARG_NOTE" ] || set -- "$@" "--note=$ACA_ARG_NOTE"
    exec "$a" "$@" $watch -- "$ACA_ARG_SAY"
    ;;
  start)
    project=$("$a" router-default "$group" dir)
    case "$ACA_ARG_DIR" in
      "") dir=$project ;;
      /*) dir=$ACA_ARG_DIR ;;
      *) dir=$project/$ACA_ARG_DIR ;;
    esac
    if [ ! -d "$dir" ]; then echo "$dir: not a directory" >&2; exit 1; fi
    tool=$ACA_ARG_TOOL
    [ -n "$tool" ] || tool=$("$a" router-default "$group" tool)
    n=$ACA_ARG_TICKET
    if [ -z "$n" ]; then
      n=$("$a" task add "-g=$group" "-t=$ACA_ARG_TITLE" "--criteria=$ACA_ARG_CRITERIA" $watch)
    fi
    set -- new "$tool" "$dir" --parent - "-g=$group" "-t=$ACA_ARG_TITLE" "--task=$n" "--say=$ACA_ARG_SAY"
    [ -z "$ACA_ARG_BRANCH" ] || set -- "$@" "-w=$ACA_ARG_BRANCH"
    if ! id=$("$a" "$@" $watch); then echo "#${n#\#}" >&2; exit 1; fi
    printf '#%s %s\n' "${n#\#}" "$id"
    ;;
  give) exec "$a" task give "$ACA_ARG_TICKET" "$ACA_ARG_TO" $watch -- "$ACA_ARG_SAY" ;;
  set)
    set -- task set "$ACA_ARG_TICKET"
    [ -z "${ACA_ARG_TITLE+x}" ] || set -- "$@" "-t=$ACA_ARG_TITLE"
    [ -z "${ACA_ARG_CRITERIA+x}" ] || set -- "$@" "--criteria=$ACA_ARG_CRITERIA"
    [ -z "${ACA_ARG_NOTE+x}" ] || set -- "$@" "--note=$ACA_ARG_NOTE"
    exec "$a" "$@" $watch
    ;;
  message) printf %s "$ACA_ARG_TEXT" | "$a" msg "$ACA_ARG_TO" ;;
  move)
    # Word splitting is intentional for this list of ids. Globbing is disabled above.
    exec "$a" move --tree $ACA_ARG_IDS "$group"
    ;;
  note) printf %s "$ACA_ARG_TEXT" | "$a" note add "-g=$group" ;;
  notes) exec "$a" note ls "-g=$group" ;;
  wake_me) printf %s "$ACA_ARG_TEXT" | "$a" msg "--in=$ACA_ARG_AFTER" "$ACA_SESSION_ID" ;;
  groups) exec "$a" group ls --routers ;;
  *) exit 2 ;;
esac
