#!/bin/sh
# aca.copy:这个会话最后一条回复里的命令 / 代码,挑一段复制。
# 认回复的原文,不认屏幕:``` 围起来的代码块(整块,几行都在)、行内的 `…`(设置 inline,默认只要带空格的);
# shell 的代码块里有几条命令的,每一条也单独列出来(设置 lines)。
#   pick    挑(弹窗里,fzf):数字键直接复制那一条,↑↓ ⏎ 复制高亮的,Esc 不复制;只认出一条就不问
#   list    打出来:认出了什么,一行一条
# 回复是 aca 给的(清单里写了 input = "reply":窗格读 ACA_INPUT_FILE,命令读标准输入)。
# 复制 = tmux 的缓冲区(前缀 ] 贴到别的格里)+ OSC 52 给外层终端(ssh 进来的也到)+ 本机的剪贴板。

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

# 一句话。弹窗一关,打在里面的字就没了:在 tmux 里写到状态栏;不在 tmux 里(手机直接 ssh 进来)停一下再回界面
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

# 每认出一段:原文写进 $tmp/<编号>(结尾不带换行:贴进 shell 不会自己回车),打一行「种类 编号 行数 第一行」
# (种类:b 代码块、l 上面那个代码块里的一条、i 行内的)。每一行都是 "$ " 开头的,去掉这个提示符;一样的只留一段。
# 代码块没收尾(回复被截在里面)的不要,留个记号
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
# shell 的代码块里的每一条命令。行尾的 \ 接着下一行;有 "$ " 开头的行就只要这些(别的是输出);
# 有缩进的(循环、条件)、有 << 的(一段文字喂给命令)拆开就不是一条了:不拆
function commands(text,    n, i, lines, cur, l, dollar, cont) {
  if (text ~ /<</) return
  n = split(text, lines, "\n")
  dollar = 0
  cont = 0
  for (i = 1; i <= n; i++) {
    if (lines[i] ~ /^\$ /) dollar = 1
    # 接着上一行 \ 的那几行缩进不算
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
# 一行里的 `…`:开头几个反引号,收尾就得是几个
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
    # 列表里的代码块跟着列表缩进:开头那一行缩了几格,里面每行去掉几格
    for (i = 0; i < ind && substr(line, 1, 1) == " "; i++) line = substr(line, 2)
    buf = buf line "\n"
    next
  }
  if (match(t, /^(```+|~~~+)/)) {
    # 反引号开的头,后面那截(语言名)里不会再有反引号:有就是一行里的 ```…```
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

# 代码块(和它里面的每一条)在前,行内的在后,各自照回复里的先后;这里的第几行就是给人看的编号
{ grep '^[bl]' "$tmp/found"; grep '^i' "$tmp/found"; } > "$tmp/index"
n=$(wc -l < "$tmp/index" | tr -d ' ')
cut_note=""
[ -f "$tmp/open" ] && cut_note=$(say "最后一个代码块不完整(回复被截断),已忽略" "the last code block is cut off (the reply was truncated) and left out")

if [ "$n" -eq 0 ]; then
  tell "$(say "最后一条回复中没有可复制的内容:没有代码块,也没有含空格的行内代码" "nothing to copy in the last reply: no code blocks, no inline code with spaces")${cut_note:+ · $cut_note}"
  exit 1
fi

# 列表:$1 = 1 时带上给 fzf 的那一列(编号)和颜色
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

# 挑第几条:打出编号;不挑了什么也不打
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
  # 你自己没配颜色的话用终端那 16 色:浅色深色的主题都看得清
  case "${FZF_DEFAULT_OPTS:-}" in *--color*) color="" ;; *) color="--color=16" ;; esac
  header=$(say "数字键 / ⏎ 复制 · Esc 取消" "digit / Enter copies · Esc cancels")
  rows 1 | fzf --ansi --delimiter='\t' --with-nth=2.. --layout=reverse --no-multi --cycle --info=hidden $color \
    --prompt="$(say "筛选 " "filter ")" --header="$header${cut_note:+ · $cut_note}" \
    --preview="cat '$tmp'/\$(sed -n {1}p '$tmp/index' | cut -f2)" --preview-window='down,50%,wrap' \
    --bind="$keys" | cut -f1
}

# 交给谁复制;一处都没成才算没成
copy() {
  if [ -n "${ACA_SETTING_COPY:-}" ]; then
    sh -c "$ACA_SETTING_COPY" < "$1"
    return
  fi
  done=1
  if [ -n "${TMUX:-}" ] && tmux load-buffer -w "$1" 2>/dev/null; then done=0; fi
  # ssh 进来的:这台机器的剪贴板不是你手边那个,只走上面的 OSC 52
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
