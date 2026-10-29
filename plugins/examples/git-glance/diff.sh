#!/bin/sh
# View the changes of the selected session in a popup; q quits
cd "$ACA_SESSION_DIR" 2>/dev/null || { echo "未选中会话(--session)"; sleep 2; exit 1; }
{
  git status --short
  echo
  git -c color.ui=always diff HEAD --stat
  echo
  git -c color.ui=always diff HEAD
} 2>&1 | less -R
