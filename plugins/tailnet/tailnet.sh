#!/bin/sh
# aca.tailnet:tailscale serve 把本机的什么挂在 tailnet 上。整个功能就是这个脚本 —— aca 里没有挂载的代码,
# 用的只是插件接口(ACA_ARG_<名字>、ACA_LANG、标准输出 / 退出码)和外面的 tailscale、netstat、tpub、jq。
# 写插件可以照着它来。
#
#   rows      列表视图要的 JSON(docs/plugins.md「列表视图的输出」)
#   list      给人、给 agent 看的几行字;tailscale 没在跑时退出码是 1
#   add       把本机端口 ACA_ARG_PORT 原样挂到 tailnet(https,tailnet 里也是这个端口号,好记)
#   drop      撤掉 ACA_ARG_TARGET(rows 里的 id,或者地址)
#   publish   ACA_ARG_TO = on / off:开关发布(外面的脚本 tpub,PATH 里有才算装了)
#
# 谁挂的都列:手敲的 tailscale serve、脚本、skill。
# - 撤的时候每条都带上端口和路径(--https=443 --set-path=/x off),不用 reset:那会把别处挂的一起清掉
# - tailscale 说成了不算:撤完、挂完都重读一遍,要动的那条对了、别的一条没少才算成
# - 前台挂的(没带 --bg 的 tailscale serve)撤不了:那条命令停了它自己就没了
# - 只管 tailnet 里的:不开 funnel(已经开到公网的只标出来,撤照样能撤)
# - 挂的时候目标写 localhost:只听 ::1 的 dev server(新一点的 Node 默认这样)写 127.0.0.1 转不过去
# - 目标在没在听,问系统(netstat)而不是自己去连:装了 LuLu 这类防火墙的机器上去连本机端口会弹窗问、一直卡着
# - 读的几样(tailscale 的状态、挂载、tpub、netstat)一起问,不一个等一个:列一次的时间是最慢那一样
set -u

# 带界面的 macOS 版自带的命令行(没往 PATH 里装 tailscale 的机器)
APP=/Applications/Tailscale.app/Contents/MacOS/Tailscale
case "${ACA_LANG:-}" in zh*) L=zh ;; *) L=en ;; esac

# 说一句:t 中文 English
t() { if [ "$L" = zh ]; then printf '%s' "$1"; else printf '%s' "$2"; fi; }
die() {
  printf '%s\n' "$1" >&2
  exit 1
}

command -v jq >/dev/null 2>&1 || die "$(t 'aca.tailnet 需要 jq,但 PATH 中找不到(macOS 15 及以上自带 /usr/bin/jq;其他系统请用包管理器安装:brew / apt / dnf install jq)' 'aca.tailnet needs jq, which is not on PATH (macOS 15+ ships /usr/bin/jq; otherwise install it with your package manager: brew / apt / dnf install jq)')"
TS=$(command -v tailscale 2>/dev/null) || TS=
[ -z "$TS" ] && [ -x "$APP" ] && TS=$APP
TPUB=$(command -v tpub 2>/dev/null) || TPUB=
TMP=$(mktemp -d "${TMPDIR:-/tmp}/aca-tailnet.XXXXXX") || exit 1
trap 'rm -rf "$TMP"' EXIT
trap 'exit 1' INT TERM

# 跑一条命令,最多等 $2 秒:输出进 $TMP/$1.out,报错进 $TMP/$1.err,退出码进 $TMP/$1.rc(等超了是 124)
run() {
  name=$1 secs=$2
  shift 2
  rm -f "$TMP/$name.late"
  "$@" </dev/null >"$TMP/$name.out" 2>"$TMP/$name.err" &
  pid=$!
  (
    trap 'kill "$s" 2>/dev/null; exit 0' TERM
    sleep "$secs" &
    s=$!
    wait "$s" && kill "$pid" 2>/dev/null && : >"$TMP/$name.late"
  ) >/dev/null 2>&1 &
  dog=$!
  wait "$pid"
  rc=$?
  kill "$dog" 2>/dev/null
  wait "$dog" 2>/dev/null
  [ -e "$TMP/$name.late" ] && rc=124
  echo "$rc" >"$TMP/$name.rc"
}

ok() { [ "$(cat "$TMP/$1.rc" 2>/dev/null)" = 0 ]; }

# 没成的那一条为什么:等超了说等了多久;不然是它报错里有用的那一行(第一行不空、不教人看 --help 的,去掉「error: 」)
why() {
  name=$1 prog=$2 secs=$3
  if [ "$(cat "$TMP/$name.rc" 2>/dev/null)" = 124 ]; then
    t "$prog 在 $secs 秒内无响应" "$prog didn't answer within $secs seconds"
    return
  fi
  line=$(cat "$TMP/$name.err" "$TMP/$name.out" 2>/dev/null | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' | grep -v -e '^$' -e '--help' | head -n 1)
  line=${line#error: }
  if [ -n "$line" ]; then printf '%s' "$line"; else t "$prog 未说明原因" "$prog didn't say why"; fi
}

# 挂载的规矩,都在 jq 里:读 tailscale 的输出、拼地址、撤 / 加回去的命令。$L 是语言
LIB='
def t($zh; $en): if $L == "zh" then $zh else $en end;
def text: if type == "string" and . != "" then . else null end;
def pad($n): if $n > 0 then " " * $n else "" end;
def hp: capture("^(?<h>.*):(?<p>[0-9]+)$") // null;

# tailscale serve status 的一份配置 → 一条条挂载(TCP 的 host 先空着;目标活着没还没查)
def mounts_of($fg):
  . as $c
  | [($c.AllowFunnel // {}) | to_entries[] | select(.value == true) | (.key | hp) | select(. != null) | .p | tonumber] as $funnel
  | ($c.TCP // {}) | to_entries[]
  | (.key | try tonumber catch null) as $port
  | select($port != null)
  | .value as $h
  | any($funnel[]; . == $port) as $public
  | if ($h.TCPForward | text) then
      {kind: (if ($h.TerminateTLS | text) then "tls" else "tcp" end), host: "", port: $port, path: "",
       to: {forward: $h.TCPForward}, public: $public, fg: $fg}
    else
      (if $h.HTTP == true then "http" else "https" end) as $kind
      | ($c.Web // {}) | to_entries[]
      | (.key | hp) as $a
      | select($a != null and ($a.p | tonumber) == $port)
      | (.value.Handlers // {}) | to_entries[]
      | .value as $hd
      | {kind: $kind, host: $a.h, port: $port, path: .key, public: $public, fg: $fg,
         to: (if ($hd.Proxy | text) then {proxy: $hd.Proxy} elif ($hd.Path | text) then {path: $hd.Path} else {text: true} end)}
    end;
# 前台的在它那条命令活着时另存一份
def parse: [mounts_of(false), ((.Foreground // {}) | .[] | mounts_of(true))] | sort_by(.port, .path, .fg);

def web: .kind == "https" or .kind == "http";
# 认这一条的名字(drop 认它,也认地址):「https:443/docs」「tcp:2222」,前台的前面加「fg:」
def key: (if .fg then "fg:" else "" end) + .kind + ":" + (.port | tostring) + .path;
def url:
  if .kind == "https" then "https://\(.host)\(if .port == 443 then "" else ":\(.port)" end)\(.path)"
  elif .kind == "http" then "http://\(.host)\(if .port == 80 then "" else ":\(.port)" end)\(.path)"
  else "\(.host):\(.port)" end;
# 列表里那一格:「:443 /」「:8080 http /」「:2222 tcp」
def listen:
  if .kind == "https" then ":\(.port) \(.path)" elif .kind == "http" then ":\(.port) http \(.path)" else ":\(.port) \(.kind)" end;
def tilde: if $home != "" and (. == $home or startswith($home + "/")) then "~" + .[($home | length):] else . end;
# 转到哪,短的写法(http:// 不写,家目录写成 ~)
def target:
  if .to.proxy then .to.proxy | ltrimstr("http://") elif .to.path then .to.path | tilde
  elif .to.forward then .to.forward else t("固定文本"; "a fixed text") end;
def state:
  (.to.path != null) as $file
  | if .up == true then (if $file then t("文件存在"; "file present") else t("监听中"; "listening") end)
    elif .up == false then (if $file then t("文件不存在"; "file gone") else t("未监听"; "not listening") end)
    else "" end;
def tags: [state, (if .public then t("公网"; "public") else "" end), (if .fg then t("前台"; "foreground") else "" end)] | map(select(. != "")) | join(" · ");
def mark: if .up == true then "●" elif .up == false then "✕" else "·" end;
def flag: "--" + (if .kind == "tls" then "tls-terminated-tcp" else .kind end) + "=" + (.port | tostring);
def sub: if .public then "funnel" else "serve" end;
# 撤这一条:tailscale 后面的参数
def off_args: [sub, flag] + (if web then ["--set-path=" + .path] else [] end) + ["off"];
# 怎么把它加回去(撤错了照这条敲);固定的字没读内容,加不回
def add_cmd:
  (if .to.proxy then .to.proxy elif .to.path then (if (.to.path | test(" ")) then "\u0027\(.to.path)\u0027" else .to.path end)
   elif .to.forward then "tcp://" + .to.forward else null end) as $to
  | if $to == null then null
    else "tailscale \(sub) --bg \(flag)\(if web and .path != "/" then " --set-path=" + .path else "" end) \($to)" end;
# 是不是同一条(目标换了也算同一条:一个端口的一个路径只挂得了一样)
def same($o): [.kind, .port, .path, .fg] == [$o.kind, $o.port, $o.path, $o.fg];

# 代理的目标 → 它听在哪:http://127.0.0.1:8420/x → 127.0.0.1:8420;没写端口的按协议补
def addr_of:
  (if test("://") then {s: (split("://")[0]), r: (split("://")[1:] | join("://"))} else {s: "http", r: .} end) as $x
  | ($x.r | split("/")[0]) as $host
  | if ($host | test(":[0-9]+$")) then $host else "\($host):\(if ($x.s | startswith("https")) then 443 else 80 end)" end;
# 这个地址(localhost:3000)有没有在听。只认本机回环的:别的机器上的问不到(null)。
# 听所有地址的算;只听在 tailscale 自己那个地址上的不算(那是 tailscale 给本机访问留的口子,不是目标)
def listening($ls):
  hp as $a
  | if $a == null or $ls == null then null else
      ($a.h | ltrimstr("[") | rtrimstr("]")) as $h | ($a.p | tonumber) as $p
      | (if $h == "127.0.0.1" then [true, false] elif $h == "::1" then [false, true] elif $h == "localhost" then [true, true] else null end) as $vv
      | if $vv == null then null
        else any($ls[]; .port == $p and (($vv[0] and (.ip == "127.0.0.1" or (.ip == "*" and .v4))) or ($vv[1] and (.ip == "::1" or (.ip == "*" and .v6)))))
        end
    end;

# tailscale status --json → 这台机器的名字、毛病
def who:
  if . == null then {host: "", err: t("无法解析 tailscale 的状态"; "can\u0027t parse tailscale\u0027s status")}
  else {host: ((.Self.DNSName // "") | rtrimstr(".")),
        err: ((.BackendState // "") as $b
          | if $b == "Running" then null
            elif $b == "NeedsLogin" or $b == "NoState" then t("tailscale 未登录"; "tailscale isn\u0027t logged in")
            elif $b == "Stopped" then t("tailscale 已关闭:挂载仍在,但无法访问"; "tailscale is off: the mounts are still there, just unreachable")
            else t("tailscale 当前状态为 \($b)"; "tailscale is \($b)") end)}
  end;

def pub_label:
  (if .count > 0 then t(" · \(.count) 项"; " · \(.count) items") else "" end) as $n
  | if .state == "on" then t("已开启\($n)"; "on\($n)")
    elif .state == "half" then t("部分开启:\(.note)"; "half on: \(.note)")
    elif .note == "" then t("已关闭\($n)"; "off\($n)")
    else t("已关闭 · \(.note)"; "off · \(.note)") end;
def publish_line: t("发布 "; "publishing ") + pub_label;
'

# 读现在的样子 → $TMP/state.json:{host, err, mounts, publish}。$1 = pub 时也问 tpub(撤、挂之后核对的不问,省一趟)
load() {
  : >"$TMP/status.out" >"$TMP/serve.out" >"$TMP/tpub.out" >"$TMP/netstat.out"
  echo 1 >"$TMP/status.rc"
  echo 1 >"$TMP/serve.rc"
  echo 1 >"$TMP/netstat.rc"
  rm -f "$TMP/tpub.rc"
  if [ -n "$TS" ]; then
    run status 3 "$TS" status --json --peers=false &
    run serve 3 "$TS" serve status --json &
  fi
  if [ "${1:-}" = pub ] && [ -n "$TPUB" ]; then
    run tpub 3 "$TPUB" status --json &
  fi
  case $(uname -s) in
    Darwin) run netstat 3 netstat -an -p tcp & ;;
    *) run netstat 3 netstat -ltn & ;;
  esac
  wait
  # 在听的:「v4 v6 地址 端口」一行一个。macOS「tcp4 0 0 127.0.0.1.8420 *.* LISTEN」,Linux「tcp6 0 0 :::22 :::* LISTEN」
  if ok netstat; then
    awk '$1 ~ /^tcp/ && $NF == "LISTEN" {
      proto = substr($1, 4); n = match($4, /[.:][0-9]+$/); if (n == 0) next
      ip = substr($4, 1, n - 1); port = substr($4, n + 1)
      v4 = (proto ~ /4/ || proto == ""); v6 = (proto ~ /6/)
      if (ip == "0.0.0.0" || ip == "::" || ip == "*") ip = "*"
      printf "%d\t%d\t%s\t%s\n", v4, v6, ip, port
    }' "$TMP/netstat.out" >"$TMP/listen.tsv"
  else
    rm -f "$TMP/listen.tsv"
  fi
  # 目标是文件 / unix socket 的:在不在(jq 看不了文件)
  : >"$TMP/exists.txt"
  if ok serve; then
    jq -r '.. | objects | ((.Path? | strings), (.Proxy? | strings | select(startswith("unix:")) | ltrimstr("unix:")))' "$TMP/serve.out" 2>/dev/null |
      while IFS= read -r p; do [ -e "$p" ] && printf '%s\n' "$p"; done >"$TMP/exists.txt"
  fi
  status_err=
  serve_err=
  tpub_err=
  if [ -z "$TS" ]; then
    status_err=$(t '未安装 tailscale' "tailscale isn't installed")
  else
    ok status || status_err=$(why status tailscale 3)
    ok serve || serve_err=$(why serve tailscale 3)
  fi
  [ -e "$TMP/tpub.rc" ] && ! ok tpub && tpub_err=$(why tpub tpub 3)
  jq -n --arg L "$L" --arg home "${HOME:-}" \
    --rawfile status "$TMP/status.out" --arg status_err "$status_err" \
    --rawfile serve "$TMP/serve.out" --arg serve_err "$serve_err" \
    --rawfile tpub "$TMP/tpub.out" --arg tpub_err "$tpub_err" --arg has_tpub "$([ -e "$TMP/tpub.rc" ] && echo 1)" \
    --arg has_ls "$([ -e "$TMP/listen.tsv" ] && echo 1)" --rawfile ls "$([ -e "$TMP/listen.tsv" ] && echo "$TMP/listen.tsv" || echo /dev/null)" \
    --rawfile exists "$TMP/exists.txt" "$LIB"'
    (if $status_err != "" then {host: "", err: $status_err} else ($status | try fromjson catch null) | who end) as $w
    | (if $has_ls == "1" then [$ls | split("\n")[] | select(. != "") | split("\t") | {v4: (.[0] == "1"), v6: (.[1] == "1"), ip: .[2], port: (.[3] | tonumber)}] else null end) as $ls
    | ($exists | split("\n") | map(select(. != ""))) as $ex
    | (if $serve_err != "" then {err: $serve_err}
       else ($serve | try (fromjson | {mounts: parse}) catch {err: t("无法解析 tailscale 的挂载"; "can\u0027t parse tailscale\u0027s mounts")}) end) as $s
    | {host: $w.host,
       err: ($w.err // $s.err),
       mounts: [($s.mounts // [])[]
         | if .host == "" then .host = $w.host else . end
         | .up = (if .to.proxy then
                    (if (.to.proxy | startswith("unix:")) then (.to.proxy | ltrimstr("unix:")) as $p | any($ex[]; . == $p)
                     else .to.proxy | addr_of | listening($ls) end)
                  elif .to.forward then .to.forward | listening($ls)
                  elif .to.path then .to.path as $p | any($ex[]; . == $p)
                  else null end)],
       publish: (if $has_tpub != "1" then null
                 elif $tpub_err != "" then {state: "off", count: 0, note: $tpub_err}
                 else ($tpub | try (fromjson | {state: (if .state == "on" or .state == "half" then .state else "off" end),
                                               count: (.count // 0), note: (.note // "")})
                       catch {state: "off", count: 0, note: t("无法解析 tpub 的状态"; "can\u0027t parse tpub\u0027s status")}) end)}
  ' >"$TMP/state.json"
}

# 在 state.json 上跑一段 jq(能用上面那些规矩)
q() { jq --arg L "$L" --arg home "${HOME:-}" "$@"; }

rows() {
  load pub
  q -c "$LIB"'
    ([.mounts[] | listen | length] | max // 0 | if . > 22 then 22 else . end) as $w
    | {head: ([.host] + (if .publish then [.publish | publish_line] else [] end) | map(select(. != "")) | join("\n")),
       warn: (.err // ""),
       empty: t("暂无挂载"; "no mounts"),
       fields: (if .publish then {publish_to: (if .publish.state == "on" then "off" else "on" end)} else {} end),
       rows: [.mounts[] | {
         id: key, mark: mark,
         tone: (if .up == true then "ok" elif .up == false then "bad" else "dim" end),
         text: (listen as $l | $l + pad($w - ($l | length)) + " → " + target),
         right: tags,
         detail: ([url, (if .to.text then t("→ 固定文本"; "→ a fixed text") else "→ " + (.to.proxy // .to.path // .to.forward) end)]
           + (if .public then [t("已开放到公网(funnel):tailnet 之外也能访问"; "open to the internet (funnel): reachable from outside the tailnet")] else [] end)
           + (if .fg then [t("前台挂载无法撤销:停止对应的 tailscale serve 命令后它会自动消失"; "a foreground mount can\u0027t be removed: stop that tailscale serve and it goes away")] else [] end)),
         fields: ({url: url} + (if .fg then {} else {drop: key} end))}]}
  ' "$TMP/state.json"
}

list() {
  load pub
  q -r "$LIB"'
    ([.mounts[] | url | length] | max // 0) as $w
    | (if .host != "" then .host else empty end),
      (if .err then "! " + .err else empty end),
      (if .publish then .publish | publish_line else empty end),
      (if (.mounts | length) == 0 and .err == null then t("暂无挂载"; "no mounts") else empty end),
      (.mounts[] | (url as $u | "\(mark) \($u)\(pad($w - ($u | length)))  → \(target)  \(tags)" | sub(" +$"; "")))
  ' "$TMP/state.json"
  [ -n "$(q -r '.host' "$TMP/state.json")" ]
}

add() {
  port=${ACA_ARG_PORT:-}
  case $port in '' | *[!0-9]*) die "$(t '端口应为 1 到 65535 之间的数字' 'a port is a number from 1 to 65535')" ;; esac
  { [ "$port" -ge 1 ] && [ "$port" -le 65535 ]; } || die "$(t '端口应为 1 到 65535 之间的数字' 'a port is a number from 1 to 65535')"
  load
  if [ -z "$(q -r '.host' "$TMP/state.json")" ]; then
    die "$(q -r --arg d "$(t 'tailscale 未登录' "tailscale isn't logged in")" '.err // $d' "$TMP/state.json")"
  fi
  if q -e --argjson p "$port" 'any(.mounts[]; .port == $p)' "$TMP/state.json" >/dev/null; then
    die "$(t "tailnet 的 :$port 已有其他挂载" "the tailnet's :$port already has something else")"
  fi
  # 不带 --yes:它要问话(比如这个 tailnet 还没开 HTTPS)就让它问不成、照实报出来,不替人答应
  run change 15 "$TS" serve --bg "--https=$port" "http://localhost:$port"
  ok change || die "$(t '挂载失败:' 'not mounted: ')$(why change tailscale 15)"
  load
  said=$(q -r --argjson p "$port" --arg idle "$(t " · 本机 $port 当前未监听" " · nothing listens on local $port right now")" "$LIB"'
    [.mounts[] | select(.port == $p and .kind == "https" and .path == "/")][0]
    | if . == null then empty else t("已挂载 "; "mounted ") + url + (if .up == false then $idle else "" end) end
  ' "$TMP/state.json")
  [ -n "$said" ] || die "$(t 'tailscale 显示已挂载,但列表中没有它' "tailscale says it's mounted, but it's not in the list")"
  printf '%s\n' "$said"
}

drop() {
  target=${ACA_ARG_TARGET:-}
  load
  m=$(q -c --arg w "$target" "$LIB"'[.mounts[] | select(key == $w or url == $w)][0] // empty' "$TMP/state.json")
  if [ -z "$m" ]; then
    # 列表上按的 x:那一行是刚才列的,这会儿已经没了就是没了;命令行敲的多半是写错了
    if [ "${ACA_INVOKED_BY:-}" = menu ]; then
      t "$target 已不存在" "$target wasn't mounted anyway"
      echo
      exit 0
    fi
    die "$(t "没有挂载 $target(可在列表中查看已有的挂载)" "no mount $target (the list shows them)")"
  fi
  if [ "$(printf '%s' "$m" | jq -r .fg)" = true ]; then
    die "$(t '前台挂载无法撤销:停止对应的 tailscale serve 命令后它会自动消失' "a foreground mount can't be removed: stop that tailscale serve and it goes away")"
  fi
  eval "set -- $(printf '%s' "$m" | q -r "$LIB"'off_args | @sh')"
  run change 15 "$TS" "$@"
  ok change || die "$(t '撤销失败:' 'not removed: ')$(why change tailscale 15)"
  cp "$TMP/state.json" "$TMP/before.json"
  load
  if q -e --argjson m "$m" "$LIB"'any(.mounts[]; same($m))' "$TMP/state.json" >/dev/null; then
    die "$(t "tailscale 显示已撤销,但它仍在挂载中;请手动撤销:tailscale $*" "tailscale says it's removed, but it's still mounted; remove it by hand: tailscale $*")"
  fi
  # 别的有没有跟着没了(前台的自己会走,不算)
  lost=$(q -r --argjson m "$m" --slurpfile before "$TMP/before.json" "$LIB"'
    . as $after
    | [$before[0].mounts[] | select((.fg | not) and (same($m) | not)) | . as $x | select(any($after.mounts[]; same($x)) | not) | add_cmd // url]
    | join(" ; ")
  ' "$TMP/state.json")
  [ -z "$lost" ] || die "$(t "已撤销,但以下挂载也一起消失了,请加回去:$lost" "removed, but these went with it; add them back: $lost")"
  printf '%s' "$m" | q -r "$LIB"'t("已撤销 "; "removed ") + url, (add_cmd // empty | t("如需加回:"; "to add it back: ") + .)'
}

publish() {
  case ${ACA_ARG_TO:-} in
    on) word=$(t 开启 'turn on') ;;
    off) word=$(t 关闭 'turn off') ;;
    *) die "$(t 'publish 只接受 on / off' 'publish takes on / off')" ;;
  esac
  [ -n "$TPUB" ] || die "$(t '未安装 tpub' "tpub isn't installed")"
  # tpub 要起服务、挂上去,头一回还要等证书
  run switch 30 "$TPUB" "$ACA_ARG_TO"
  ok switch || die "$(t "无法${word}:" "couldn't $word: ")$(why switch tpub 30)"
  # 再问一遍它现在怎样,对了才算成
  run tpub 3 "$TPUB" status --json
  if ok tpub; then
    now=$(q -R -s -c "$LIB"'try (fromjson | {state: (.state // "off"), count: (.count // 0), note: (.note // "")})
      catch {state: "off", count: 0, note: t("无法解析 tpub 的状态"; "can\u0027t parse tpub\u0027s status")}' "$TMP/tpub.out")
  else
    now=$(jq -nc --arg n "$(why tpub tpub 3)" '{state: "off", count: 0, note: $n}')
  fi
  if [ "$(printf '%s' "$now" | jq -r .state)" != "$ACA_ARG_TO" ]; then
    label=$(printf '%s' "$now" | q -r "$LIB"'pub_label')
    die "$(t "已请求 tpub ${word},但当前状态为:$label" "asked tpub to $word, but now it's: $label")"
  fi
  said=$(sed 's/^[[:space:]]*//; s/[[:space:]]*$//' "$TMP/switch.out" | grep -v '^$' | head -n 1)
  [ -n "$said" ] || said=$(if [ "$ACA_ARG_TO" = on ]; then t '发布已开启' 'publishing is on'; else t '发布已关闭' 'publishing is off'; fi)
  printf '%s\n' "$said"
}

case "${1:-}" in
  rows | list | add | drop | publish) "$1" ;;
  *) die "aca.tailnet: no command ${1:-}" ;;
esac
