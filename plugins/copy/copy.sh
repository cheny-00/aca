#!/bin/sh
# aca.copy: pick a command / some code from the last reply of this session and copy it.
# Reads the original text of the reply, not the screen: code blocks fenced with ``` (whole, every line), inline `…` (the inline
# setting; by default only those with a space in them);
# a shell code block holding several commands also lists each command on its own (the lines setting).
#   pick    pick (in a popup, fzf): a number key copies that one right away, ↑↓ ⏎ copies the highlighted one, Esc copies
#           nothing; with only one found, no question is asked
#   list    print what was found, one per line
# The reply comes from aca (the manifest says input = "reply": a pane reads ACA_INPUT_FILE, a command reads stdin).
# Copy = the tmux buffer (prefix ] pastes it into another pane) + OSC 52 to the outer terminal (reaches it over ssh too) + the
# local clipboard.

zh() { [ "${ACA_LANG:-en}" = zh ]; }
say() { if zh; then printf '%s\n' "$1"; else printf '%s\n' "$2"; fi; }

mode=${1:-}
case "$mode" in
  pick | list) ;;
  *)
    say "无法识别的用法:$mode" "unknown usage: $mode"
    exit 2
    ;;
esac

tmp=$(mktemp -d "${TMPDIR:-/tmp}/aca-copy.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT
trap 'exit 130' INT TERM HUP

# One line of message. Once the popup closes, whatever was printed in it is gone: in tmux, write it to the status bar; outside
# tmux (a phone that came in straight over ssh), pause a moment before going back to the UI
tell() {
  if [ "$mode" != pick ]; then
    printf '%s\n' "$1"
  elif [ -n "${TMUX:-}" ]; then
    tmux display-message -- "$(printf '%s' "$1" | sed 's/%/%%/g; s/#/##/g')"
  else
    printf '%s\n' "$1"
    sleep 1
  fi
}

src="$tmp/reply"
if [ -n "${ACA_INPUT_FILE:-}" ]; then cat "$ACA_INPUT_FILE" > "$src" 2>/dev/null; else cat > "$src"; fi
if ! grep -q '[^[:space:]]' "$src"; then
  if [ -z "${ACA_SESSION_ID:-}" ]; then
    tell "$(say "当前窗格不是 aca 的会话" "this pane isn't an aca session")"
  else
    tell "$(say "这个会话还没有回复" "this session has no reply yet")"
  fi
  exit 1
fi

# For each piece found: write its original text to $tmp/<NUMBER> (no trailing newline: pasted into a shell, it does not press
# Enter by itself), and print a line "KIND NUMBER LINES FIRST-LINE"
# (KIND: b a code block, l one command from the code block above, i inline). When every line starts with "$ ", drop that
# prompt; identical pieces are kept once.
# A code block that never closes (the reply was cut off inside it) is dropped, leaving a marker
awk -v dir="$tmp" -v mode="${ACA_SETTING_INLINE:-spaced}" -v split_lines="${ACA_SETTING_LINES:-true}" '
function emit(text, kind,    n, i, lines, all, first) {
  gsub(/^\n+|\n+$/, "", text)
  if (text !~ /[^ \t\n]/) return
  n = split(text, lines, "\n")
  all = 1
  for (i = 1; i <= n; i++) if (lines[i] != "" && lines[i] !~ /^\$ /) { all = 0; break }
  if (all) {
    text = ""
    for (i = 1; i <= n; i++) { sub(/^\$ /, "", lines[i]); text = text (i > 1 ? "\n" : "") lines[i] }
  }
  if (text in seen) return
  seen[text] = 1
  id++
  printf "%s", text > (dir "/" id)
  close(dir "/" id)
  first = lines[1]
  gsub(/\t/, " ", first)
  printf "%s\t%d\t%d\t%s\n", kind, id, n, first
}
# Each command in a shell code block. A \ at the end of a line continues onto the next; if some lines start with "$ ", take only
# those (the rest is output);
# a block with indentation (loops, conditionals) or with << (text fed to a command) is no longer one command once split: not split
function commands(text,    n, i, lines, cur, l, dollar, cont) {
  if (text ~ /<</) return
  n = split(text, lines, "\n")
  dollar = 0
  cont = 0
  for (i = 1; i <= n; i++) {
    if (lines[i] ~ /^\$ /) dollar = 1
    # Indentation on the lines that continue a \ from the line above does not count
    if (!cont && lines[i] ~ /^[ \t]+[^ \t]/) return
    cont = (lines[i] ~ /\\$/)
  }
  cur = ""
  for (i = 1; i <= n; i++) {
    l = lines[i]
    if (cur == "") {
      if (dollar) {
        if (l !~ /^\$ /) continue
        sub(/^\$ /, "", l)
      }
      if (l ~ /^[ \t]*(#|$)/) continue
    }
    cur = (cur == "" ? l : cur "\n" l)
    if (l ~ /\\$/) continue
    emit(cur, "l")
    cur = ""
  }
  if (cur != "") emit(cur, "l")
}
# Inline `…` in a line: it must close with as many backticks as it opens with
function inline(s,    n, body, found) {
  while (match(s, /`+/)) {
    n = RLENGTH
    s = substr(s, RSTART + n)
    body = ""
    found = 0
    while (match(s, /`+/)) {
      if (RLENGTH == n) {
        body = body substr(s, 1, RSTART - 1)
        s = substr(s, RSTART + n)
        found = 1
        break
      }
      body = body substr(s, 1, RSTART + RLENGTH - 1)
      s = substr(s, RSTART + RLENGTH)
    }
    if (!found) return
    gsub(/^[ \t]+|[ \t]+$/, "", body)
    if (mode == "all" || body ~ /[ \t]/) emit(body, "i")
  }
}
{
  line = $0
  sub(/\r$/, "", line)
  t = line
  sub(/^[ \t]+/, "", t)
  if (fence != "") {
    c = t
    sub(/[ \t]+$/, "", c)
    if (length(c) >= flen && (fence == "`" ? c ~ /^`+$/ : c ~ /^~+$/)) {
      emit(buf, "b")
      if (split_lines == "true" && lang ~ /^(sh|bash|zsh|fish|shell|console|terminal)$/) commands(buf)
      fence = ""
      next
    }
    # A code block in a list is indented along with the list: strip from each line inside as many spaces as the opening line has
    for (i = 0; i < ind && substr(line, 1, 1) == " "; i++) line = substr(line, 2)
    buf = buf line "\n"
    next
  }
  if (match(t, /^(```+|~~~+)/)) {
    # A fence opened with backticks has no more backticks in the rest (the language name): if it does, it is a ```…``` on one line
    if (substr(t, 1, 1) == "~" || index(substr(t, RLENGTH + 1), "`") == 0) {
      fence = substr(t, 1, 1)
      flen = RLENGTH
      ind = length(line) - length(t)
      lang = tolower(substr(t, flen + 1))
      gsub(/^[ \t]+|[ \t].*$/, "", lang)
      buf = ""
      next
    }
  }
  if (mode != "none") inline(line)
}
END { if (fence != "") print "" > (dir "/open") }
' "$src" > "$tmp/found"

# Code blocks (with each command inside them) first, inline ones after, each in the order of the reply; the line number here
# is the number people see
{ grep '^[bl]' "$tmp/found"; grep '^i' "$tmp/found"; } > "$tmp/index"
n=$(wc -l < "$tmp/index" | tr -d ' ')
cut_note=""
[ -f "$tmp/open" ] && cut_note=$(say "最后一个代码块不完整(回复被截断),已忽略" "the last code block is cut off (the reply was truncated) and left out")

if [ "$n" -eq 0 ]; then
  tell "$(say "最后一条回复中没有可复制的内容:没有代码块,也没有含空格的行内代码" "nothing to copy in the last reply: no code blocks, no inline code with spaces")${cut_note:+ · $cut_note}"
  exit 1
fi

# The list: with $1 = 1, include the column for fzf (the number) and colors
rows() {
  awk -F'\t' -v fzf="$1" -v more="$(say "行" "more")" '{
    first = ($1 == "l" ? "  └ " : "") $4
    for (i = 5; i <= NF; i++) first = first " " $i
    extra = $3 > 1 ? sprintf("  +%d %s", $3 - 1, more) : ""
    if (fzf) printf "%d\t%2d  %s\033[2m%s\033[0m\n", NR, NR, first, extra
    else printf "%2d  %s%s\n", NR, first, extra
  }' "$tmp/index"
}

if [ "$mode" = list ]; then
  rows 0
  [ -n "$cut_note" ] && printf '%s\n' "$cut_note"
  exit 0
fi

# Pick one: print its number; print nothing when nothing is picked
choose() {
  if [ "$n" -eq 1 ]; then
    echo 1
    return
  fi
  if ! command -v fzf >/dev/null 2>&1; then
    rows 0 >&2
    printf '%s' "$(say "输入编号(回车取消):" "which one (Enter to skip): ")" >&2
    read -r k
    echo "$k"
    return
  fi
  keys=""
  i=1
  while [ "$i" -le "$n" ] && [ "$i" -le 9 ]; do
    keys="$keys${keys:+,}$i:become(echo $i)"
    i=$((i + 1))
  done
  # Unless you set colors yourself, use the 16 colors of the terminal: readable on both light and dark themes
  case "${FZF_DEFAULT_OPTS:-}" in *--color*) color="" ;; *) color="--color=16" ;; esac
  header=$(say "数字键 / ⏎ 复制 · Esc 取消" "digit / Enter copies · Esc cancels")
  rows 1 | fzf --ansi --delimiter='\t' --with-nth=2.. --layout=reverse --no-multi --cycle --info=hidden $color \
    --prompt="$(say "筛选 " "filter ")" --header="$header${cut_note:+ · $cut_note}" \
    --preview="cat '$tmp'/\$(sed -n {1}p '$tmp/index' | cut -f2)" --preview-window='down,50%,wrap' \
    --bind="$keys" | cut -f1
}

# Hand it over for copying; it counts as failed only if every one of them failed
copy() {
  if [ -n "${ACA_SETTING_COPY:-}" ]; then
    sh -c "$ACA_SETTING_COPY" < "$1"
    return
  fi
  done=1
  if [ -n "${TMUX:-}" ] && tmux load-buffer -w "$1" 2>/dev/null; then done=0; fi
  # Over ssh: the clipboard of this machine is not the one at your hand, so only the OSC 52 above is used
  if [ -z "${SSH_CONNECTION:-}${SSH_TTY:-}" ]; then
    for c in pbcopy wl-copy "xclip -selection clipboard" "xsel -ib"; do
      command -v "${c%% *}" >/dev/null 2>&1 || continue
      $c < "$1" 2>/dev/null && done=0
      break
    done
  fi
  return $done
}

k=$(choose)
case "$k" in '' | *[!0-9]*) exit 0 ;; esac
row=$(sed -n "${k}p" "$tmp/index")
[ -n "$row" ] || exit 0
file="$tmp/$(printf '%s' "$row" | cut -f2)"
first=$(printf '%s' "$row" | cut -f4-)
lines=$(printf '%s' "$row" | cut -f3)
[ "$lines" -gt 1 ] && first="$first $(say "…(共 $lines 行)" "… ($lines lines)")"
if copy "$file"; then
  tell "$(say "已复制:" "copied: ")$first"
else
  tell "$(say "无法复制:" "couldn't copy: ")$first"
  exit 1
fi
