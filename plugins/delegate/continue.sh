#!/bin/sh
# 派出去的 worker(有 parent 的会话)一轮出错、是临时的(限流、过载、断流,turn.failed 的 kind = transient):
# 等一阵(设置 delay 秒 × 第几次,默认 30),它还停在出错上,就替它说一句(设置 say,默认 continue)。同一个 worker 最多 max_continue 次
# (默认 5),跑完一轮正常的就清零。账号问题、再试也一样的不碰,交给派它的那个(它在 aca wait 上会看到)。
# 你自己开的会话(没有 parent)不碰。清单里的 if 已经只让这几种进来,下面再看一遍。
# 设置在 plugins.toml 里改;环境变量 ACA_DELEGATE_MAX_CONTINUE / ACA_DELEGATE_DELAY 压过设置(测试用)。

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
  echo "$id:已经替它续了 $n 次,不再续"
  exit 0
fi
n=$((n + 1))
mkdir -p "$ACA_PLUGIN_STATE_DIR"
echo "$n" > "$count"

sleep $((${ACA_DELEGATE_DELAY:-${ACA_SETTING_DELAY:-30}} * n))
# 这期间有人接手了(派它的那个 send 过、你在里面打过字):不再插嘴
state=$("$ACA_BIN" show "$id" --json | jq -r '.state // ""')
if [ "$state" != "error" ]; then
  echo "$id:现在是 $state,不用续了"
  exit 0
fi
say="${ACA_SETTING_SAY:-continue}"
echo "$id:第 $n 次替它说「${say}」"
"$ACA_BIN" send "$id" -m "$say"
