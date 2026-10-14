#!/bin/sh
# 会话目录里相对 HEAD 的改动 → `aca meta set <会话> git "2 个文件 +12 -3"`;没有改动 / 不是仓库就拿掉
[ -n "$ACA_SESSION_ID" ] && cd "$ACA_SESSION_DIR" 2>/dev/null || exit 0
stat=$(git diff HEAD --numstat 2>/dev/null | awk '{a += $1; d += $2; n++} END {if (n) printf "%d 个文件 +%d -%d", n, a, d}')
if [ -n "$stat" ]; then
  "$ACA_BIN" meta set "$ACA_SESSION_ID" git "$stat"
else
  "$ACA_BIN" meta rm "$ACA_SESSION_ID" git
fi
