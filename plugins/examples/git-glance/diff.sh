#!/bin/sh
# 弹窗里看选中会话的改动;q 退出
cd "$ACA_SESSION_DIR" 2>/dev/null || { echo "没有选中会话(--session)"; sleep 2; exit 1; }
{
  git status --short
  echo
  git -c color.ui=always diff HEAD --stat
  echo
  git -c color.ui=always diff HEAD
} 2>&1 | less -R
