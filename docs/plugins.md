# 写一个 aca 插件

插件是一个目录:一份 `aca-plugin.toml` 清单,加上它要跑的命令。用什么语言写都行,没有 SDK。
清单声明它带了什么,aca 负责在对的时候把命令跑起来、把给 agent 的东西带进去;插件要做什么就回头调 `aca` 命令行 ——
**命令行就是插件的 API**。

插件往两个方向加东西:给 aca 自己(界面里的键、菜单、窗格,命令行,事件钩子,定时),给 agent(skill、斜杠命令、MCP 工具)。

做法照的是 [herdr](https://github.com/herdrdev/herdr) 的插件。区别在最后一节。

插件说它想要什么,你说了算(像 kitty:kitten 不自己绑键,远程控制每个口令能用哪些命令由你列):给 agent 哪些工具、
`:` 菜单里的键,都在 `plugins.toml` 里改(见「你说了算」)。不想建插件:`~/.config/aca/user/aca-plugin.toml`
写上就有(见「你自己的插件」)。

## 写第一个插件

```sh
aca plugin new ./hello            # 建好清单和脚本:一条命令、一个键、一个设置,事件钩子写好了注释着
aca plugin check ./hello          # 不用链接也能查:清单写得对不对、要跑的程序 / 脚本在不在、needs 里的程序有没有
aca plugin link ./hello           # 链接上:改了脚本马上生效,改了清单再 link 一次
aca plugin run me.hello.hello     # 跑一下(界面里:会话列表上按 : 再按 h)
aca plugin emit turn.done --plugin me.hello   # 假装来了一个事件:钩子在前台跑、输出直接打出来,不用真跑 agent
aca plugin log me.hello           # 平时钩子的输出在这
```

写好了推到 GitHub,别人 `aca plugin install 你/仓库` 就装上了。

照着抄的例子:[`plugins/tailnet/`](../plugins/tailnet)(`aca.tailnet`)整个功能就在一个 sh + jq 脚本里 ——
列表视图、带参数的命令、给 agent 的工具、改完重读核对、中英文都有,aca 里没有一行挂载的代码。
[`plugins/remote/`](../plugins/remote)(`aca.remote`)也是:设置(带口令的)、定时(有活才开)、往会话的收件箱里发消息。
小的:[`plugins/examples/git-glance/`](../plugins/examples/git-glance)(事件 + 动作 + 窗格 + 行上的字)。

## 清单

```toml
id = "example.git-glance"        # 字母、数字、. : _ -
name = "Git Glance"
version = "0.1.0"
api = 1                          # 照着插件接口第几版写的(见「接口约定」);不写 = 1。比现在的 aca 新就读不进来
min_aca_version = "0.1.0"        # 可选;比现在的 aca 新就不让链接
description = "每轮跑完,在会话行上挂一段改动统计"
needs = ["git"]                  # 可选:要用到的外面的程序(PATH 里找,"a|b" = 有一个就行)。aca 不替它装,
                                 # 装的时候、aca plugin check、aca plugin ls、插件页说这台机器上缺了哪个
requires = ["aca.core"]          # 可选:要先有的插件,只能是 aca 自带的第一方插件,装它时一起装上
                                 # (顶上这些要写在第一个 [表头] 前面:TOML 里写在表头后面的都归那张表)

[settings.max_lines]             # 设置:默认值和说明;值写在 plugins.toml,交给命令时在环境里(ACA_SETTING_MAX_LINES)
default = 20                     # 值照默认值的类型读:界面、aca plugin setting 里填 "20x" 会说要是整数
desc = "改动统计最多列几行"
# secret = true                  # webhook 地址、key 这种:界面和 aca plugin setting 列的时候打码
                                 # (值还是明文记在 plugins.toml 里,所以 aca 把那个文件写成只有你能读的 0600)

[[events]]                       # 事件钩子:某件事发生时跑一条命令,不等它
on = "turn.done"                 # 也可以写 "turn.*"、"*"
if = { parent = true }           # 可选:对不上的不起进程(见下面「事件钩子的 if」)
command = ["sh", "stat.sh"]

[[commands]]                     # 能力:一条命令(老写法 [[actions]] 照认)
id = "refresh"                   # 字母、数字、: _ -(不能有点)
title = "刷新改动统计"
desc = "重新算一遍这个会话的改动统计"   # 给 agent 看的说明;不写用 title
contexts = ["session"]           # session / group / global:在什么上面才有意义
args = { path = "path?", top = "int?" }   # 参数:string / path / int / bool,带 ? 的可以不给
expose = ["menu", "cli"]         # 露给谁:menu(界面菜单、键)、cli(aca plugin run)、agent(给 agent 当工具)、
                                 # agent?(能给 agent,但默认不给:会动机器上东西的写这个,用户点了名才给)。不写 = menu + cli
input = "reply"                  # 可选:把会话的什么交给它 —— reply 最后一条回复 / screen 屏幕上现在的字 / scrollback 连回滚一起
                                 # (命令、视图从标准输入读;窗格读 ACA_INPUT_FILE 那个文件)
cwd = "session"                  # 可选:在会话的目录里跑(lazygit 这种);不写 = 插件目录。./x 照样按插件目录找
command = ["sh", "stat.sh"]

[[commands]]
id = "close"                     # 下面列表视图的键跑的就是它
title = "关掉 PR"
args = { number = "string" }
expose = ["cli"]
command = ["sh", "close.sh"]

[[commands]]
id = "fetch"                     # 下面的定时跑的就是它
title = "拉一下远端"
expose = ["cli"]
command = ["sh", "fetch.sh"]

[[timers]]                       # 定时:每隔一阵跑这个插件的一个能力(见下面「定时」)
run = "fetch"                    # 能力的 id:不能有必填的参数,不能只在会话上(到点跑的时候没有会话)
every = "30m"                    # 30s / 10m / 2h / 1d,最短 1m
# manual = true                  # 平时关着,插件自己 aca plugin timer on / off(有活才开);不写 = 插件开着就开着

[[panes]]                        # 窗格:一个终端程序
id = "diff"
title = "改动"
contexts = ["session"]
placement = "popup"              # popup(默认)/ split / window
width = "90%"                    # 弹窗的大小:格数或百分比
height = "90%"
command = ["sh", "diff.sh"]

[[views]]                        # 视图:一条命令的输出,放进 aca 的全屏阅读页(翻页、复制;手机上好用)
id = "ci"
title = "CI 状态"
contexts = ["session"]
command = ["sh", "ci.sh"]

[[views]]                        # 列表视图:命令输出一份 JSON(格式见「列表视图」),一行一条,每行能按 keys 里的键
id = "prs"
title = "PR"
rows = true
command = ["sh", "prs.sh"]

[[views.keys]]
key = "enter"                    # 一个字符,或者 enter;j k q 留给界面
title = "复制地址"
copy = "{url}"                   # 复制:模板里 {名字} 换成这一行的字段

[[views.keys]]
key = "x"
title = "关掉"
run = "close"                    # 带着这一行跑这个插件的一个能力(露没露给菜单都行)
args = { number = "{id}" }       # 参数从这一行填;能力要的别的参数先弹表单问
confirm = true                   # 按两次才算
# row = false                    # 不用停在哪一行上(新建一条这种);模板读列表上的 fields

[[keys]]                         # 界面 : 菜单里按这个键直接跑(能力 / 窗格 / 视图的 id)
key = "c"
run = "ci"

[[hotkeys]]                      # 热键:在会话里面不按前缀直接按(往上翻着看时也行),开一个窗格。见「热键」
key = "C-M-g"                    # tmux 的键名,要带 Ctrl / Alt(C-]、M-y、C-M-g),或者 F1–F12
run = "diff"                     # 窗格的 id(热键只开窗格)

[agent]                          # 给 agent 带东西进去
for = ["claude", "codex"]        # 给哪几家;不写 = 都给
dir = "agent"                    # 一个 Claude 格式的插件目录(skill、斜杠命令),插件目录里的相对路径
# auto = "auto"                  # 一个开关设置的名字:它开着时 agent 自己也能用 dir 里的命令(见下)

[agent.mcp]                      # 一个 MCP 服务(stdio)
command = ["./mcp-server"]       # 相对插件目录
via = "auto"                     # auto:gateway 开着就登记到 gateway,不然直接挂到 agent 上;direct / gateway 强制
scope = "shared"                 # 在 gateway 里:shared 大家共用一份 / session 每个会话一份(有状态的)
```

`command` 是参数数组,不经过 shell,工作目录是插件目录。

清单里不认识的字段不拦着链接,但会点名提醒(链接时、`aca plugin check`、插件页):多半是拼错了,或者写到了别的表底下
—— TOML 里写在 `[[keys]]` 这种表头后面的字段都归那一条。

`[agent] dir`:aca 起 agent 时带上这个目录,只对那一次启动有效,不装进它的配置目录、不碰你的设置。
插件停用 / 删掉后,agent 下次启动就没有了(正开着的要重启)。目录是一个普通的 Claude Code 插件
(`.claude-plugin/plugin.json` 加 `commands/`、`skills/` ……)。命令文件头上写 `disable-model-invocation: true` 的,
只在你敲那个斜杠命令时才进上下文。老写法 `[agents.claude] plugin_dir = "claude"` 照认(只给 Claude)。

`[agent] auto`:想让人自己定「agent 能不能自己用这些命令」时写。它指 `[settings]` 里一个默认值是 `true` / `false` 的设置
(插件页里 ⏎ 一下就切,或者 `aca plugin setting <id> <名字> on|off`)。开着:Claude 拿到的是 aca 做的一份副本,命令、skill 头上的
`disable-model-invocation` 去掉了,它的描述进了 Claude 的列表,你说一句它就会用;关着:照文件写的来。不写 `auto` = 一直照文件写的来。
只管 Claude(Codex 的 skill 本来就都在它的列表里),改了对新起、重启的会话生效。`aca.delegate` 的 `auto` 设置就是这个。

### 怎么带进 agent(挂载)

全在启动时带,只对那一次有效;不改账号的配置文件,不往你的仓库里写。没有开着的插件给这个会话东西,命令行一个参数都不多。

| | Claude | Codex |
|---|---|---|
| 插件目录(skill、命令) | `--plugin-dir <目录>`;`[agent] auto` 的设置开着时,目录换成 `~/.local/share/aca/claude-plugins/<名字>/local/` 里的副本 | aca 在 `~/.local/share/aca/codex-plugins/<名字>/local/` 放一份它认得的副本(斜杠命令转成 skill,Codex 没有自定义斜杠命令),每个账号的 `CODEX_HOME/plugins/cache/aca` 链到那里(只建一次,不改 config.toml),起的时候 `-c plugins.<名字>@aca.enabled=true` |
| 插件的 MCP 服务 | 写进一份 `--mcp-config`(在 `~/.local/state/aca/hooks/mcp-*.json`) | `-c mcp_servers.aca-<名字>={…}`,会话 id 写在 `env` 里(Codex 给 MCP 子进程的环境只有白名单) |
| 露给 agent 的能力 | MCP 服务 `aca`(`aca plugin tools`),工具名 `mcp__aca__<插件>__<能力>` | 同一个服务,`-c mcp_servers.aca={…}` |

- `<名字>` 是插件 id 里的点换成 `-`(`aca.delegate` → `aca-delegate`)。
- 两家的插件 MCP 都是 `aca plugin mcp <插件 id>`:aca 切到插件目录、带上插件的环境(设置、`ACA_SESSION_ID`)再换成 `[agent.mcp] command`。
- Codex 那条路靠的是它内部的插件缓存布局,不是公开的接口。`CODEX_HOME/plugins/cache/aca` 那里已经有别的东西时不碰它,
  这回只挂 MCP,原因记在 `~/.local/state/aca/plugin-logs/mount.log`。Codex 的 skill 没有「只在敲的时候才进上下文」这一说:
  skill 的名字和描述会在它的 skill 列表里。
- 改了插件、开停、plugins.toml 里的 agents:正开着的会话要重启才变(Codex 只在启动时读插件和工具列表)。

### 能力给 agent 当工具

`expose` 里有 `agent` 的能力(还有你点了名的 `agent?`,见「你说了算」),aca 自带的 MCP 服务 `aca` 把它们包成工具:参数照 `args` 生成说明(`string` / `path` → 字符串,
`int` → 整数,`bool` → 布尔,不带 `?` 的必填),说明用 `desc`(没写用 `title`)。agent 调的时候 aca 照类型查参数、
以 `ACA_PLUGIN_ARGS_JSON` 交给命令、`ACA_SESSION_ID` 是在调的那个会话,标准输出和标准错误原样交回;退出码不是 0 的标成出错。
一次最多跑 280 秒(Codex 默认 300 秒不等):要等更久的,让命令先回「还在跑」,agent 下次再问。
列出来的只有够得着这个会话的插件的(plugins.toml 的 `agents`)。

### 事件钩子的 `if`

键是事件里的字段(`kind`,嵌套的写 `data.x`),加上这个会话的几样:`parent`(派它的那个)、`tool`、`account`、`group`。
值相等才跑;写数组 = 其中一个就行;`true` / `false` = 有没有(非空)。比如只管派出去的 worker 的临时错误:
`if = { kind = "transient", parent = true }`。对不上的连进程都不起。

### 热键

`[[keys]]` 是界面里 `:` 菜单的键;`[[hotkeys]]` 是你人在会话里面(对着 agent)时按的。aca 把它绑在 tmux 上,
和进去之后 Ctrl+Q 回来是一个办法:

- **只在 aca 的会话里拦**。别的 tmux 会话里这个键照样送给里面的程序。
- **不用前缀,翻着看时也能按**:绑在平常的键表和 copy-mode 的两张键表里。按了就开那个窗格,冲着按键的那个会话
  (`ACA_SESSION_ID`)、弹在按键的那台终端上;窗格写了 `input` 的照样拿得到会话的内容。
- **装上就有,去掉就没**:绑定不写进 tmux.conf。插件装上 / 打开时绑,停掉 / 删掉时解;tmux 重启了,下次建会话、
  进会话时补上。
- **不抢你的键**:那个键你自己在 tmux 里绑了别的,aca 不动它,`aca plugin hotkey` 里点名;tmux 自带的翻看用的键也不盖。

键写 tmux 的键名,要带 Ctrl / Alt:`C-]`、`M-y`、`C-M-g`,或者 `F1`–`F12`。光一个字符的不行(会把你打的字截走);
`C-q`(aca 自己的)、`C-c` `C-d` `C-z` 这些终端里另有意思的不行,它们加上 Alt 的(`C-M-c`)也不行:绑定还没补上的时候,
这个键发给 agent 的就是 Esc 加 Ctrl+C。一个窗格一个热键。
挑键的时候留意:Ctrl 加字母差不多都被 agent 的输入框用了(Claude Code 连 `C-]` 都用了),Alt 加字母常被窗口管理器截走;
在 copy-mode 的键表里已经有用处的键(vi 键位下的 `M-y`),翻着看的时候按的还是原来那个用处。

## 你说了算

插件在清单里说它想要什么,你在 `plugins.toml` 里定(命令行、插件页也能改,都写进这份文件):

```toml
[plugins."aca.tailnet"]
tools = ["list", "add"]            # 哪些能力给 agent 当工具。不写 = 清单里写 agent 的那些;写 agent? 的要在这里点名
keys = { mounts = "M", run = "" }  # : 菜单里的键:能力 / 窗格 / 视图 → 一个字符,"" = 不要键;没写的照清单的 [[keys]]
hotkeys = { mounts = "M-m" }       # 会话里面的热键:窗格 → tmux 的键名,"" = 不要;没写的照清单的 [[hotkeys]]
```

- **给 agent 的工具**:aca 列工具、真去调的时候都照 `tools` 查,不在里面的 agent 看不到、调不了。清单里写 `agent?` 的
  (挂端口、删东西这种会动你机器的)默认不给。改了之后开着的会话要重启才变(Codex 只在启动时读工具;gateway 开着时
  Claude 当场换)。这是给 agent 的工具清单,**不是沙箱**:agent 有 shell 的话照样能敲 `aca` 命令。
- **键**:两个插件要同一个键时,你写的先拿;都是清单写的,先登记的那个拿。没拿到的不再悄悄没了 ——
  `aca plugin ls`、`aca plugin key`、插件页里点名,告诉你在哪改。`/` 是 `:` 菜单自己的(筛),给不了。
- **热键**:分法一样(你写的先拿)。清单没给热键的窗格你也能给一个 —— 哪个插件的哪个窗格都行。

```sh
aca plugin allow aca.tailnet             # 列出能给 agent 的:✓ 给了 ○ 没给(要你点名的标出来)
aca plugin allow aca.tailnet add         # 给;deny 收回;--reset 回到清单的
aca plugin key                           # : 菜单里全部的键,没拿到键的点名
aca plugin key aca.tailnet mounts M      # 换成 M;--none 不要键;--reset 回到清单的
aca plugin hotkey                        # 会话里面全部的热键,没绑上的点名
aca plugin hotkey aca.copy pick M-y      # 换成 Alt+y(tmux 上当场换);--none 不要;--reset 回到清单的
```

界面里:插件页(`P`)右边是高亮那个插件的详情,⏎ 进去改 —— 设置(⏎ 改、开关类空格切换、`-` 回到默认)、
给 agent 的工具(空格切换)、插件菜单(会话列表上按 `:` 出来的那个)里的键(⏎ 再按一个键换成它,⌫ 不要键,
`-` 回到清单的),Esc 回列表。

## 你自己的插件

不想建插件、不想 link:写一份 `~/.config/aca/user/aca-plugin.toml`,放了就有,删了就没(像在 kitty.conf 里写一行 `map`)。
写法跟别的插件一样,id 总是 `user`,name、version 可以不写;脚本放在旁边(`./x.sh` 按这个目录找)。改了马上生效,不用 sync。

```toml
# ~/.config/aca/user/aca-plugin.toml
[[panes]]
id = "lazygit"
title = "lazygit"
cwd = "session"                  # 在会话的目录里开
width = "95%"
height = "95%"
command = ["lazygit"]

[[commands]]
id = "links"
title = "回复里的链接"
input = "reply"                  # 最后一条回复从标准输入进来
command = ["sh", "-c", "grep -oE 'https?://[^ )]+' | sort -u"]

[[keys]]
key = "g"
run = "lazygit"
```

plugins.toml 里不用写它(sync 不装它也不删它);要停着、改它给 agent 的工具或键,照样写 `[plugins.user]`。
`aca plugin remove user` 删不掉它:删那份文件。

## 界面里

- **`:` 插件菜单**:列表上按 `:`,列出开着的插件露给 `menu` 的能力、窗格、视图,按高亮的那行挑 `contexts` 对得上的
  (会话上是 `session`,分组标题上是 `group`,别处是 `global`;不写 `contexts` = 哪儿都行)。↑↓ 挑、⏎ 跑,
  `[[keys]]` 绑了键的直接按那个键,`/` 筛。冲着高亮的会话跑:`ACA_SESSION_ID` 是它。
- **参数表单**:能力有 `args` 的先弹一张表,一个参数一格(`bool` 用空格切换,带 `?` 的可以不填),照类型查过才跑。
- **输出**:能力跑完,输出一行的写在底栏,长的放进全屏阅读页;视图总是放进阅读页,`r` 重跑、`c` 复制、`Esc` 回来。
  都在后台跑(最多 120 秒),不挡别的操作。
- **窗格**:开在 tmux 弹窗 / 分屏 / 新窗口里;界面不在 tmux 里(手机直接 ssh 进来)就让出终端在前台跑。
- **列表视图**(`rows = true`):开成一页,一行一条,下面是高亮那行的细节,底下写着这一行能按的键。模板里用到的字段
  这一行没有(或是空的),这个键在这一行上就不列、按了说按不了。要按两次的先提醒。键跑完,输出写在列表下面
  (比如撤掉之后怎么加回去),列表重读;`r` 重读(插件没把 `r` 占了的话)。
- **行上的字**:`aca meta set` 挂的字在列表里跟在标题后面,灰色的 `[名字 值]`。

### 列表视图的输出

```json
{
  "head": "mac.tail.ts.net\n发布 关着",          // 列表上面的灰字(可以几行)
  "warn": "tailscale 没登录",                     // 要留意的(警告色)
  "empty": "没有挂载",                            // 一行都没有时写的
  "fields": { "publish_to": "on" },               // row = false 的键读这里
  "rows": [
    { "id": "https:443/", "mark": "●", "tone": "ok", "text": ":443 / → localhost:3000", "right": "在听",
      "detail": ["https://mac.tail.ts.net/"], "fields": { "url": "https://mac.tail.ts.net/" } }
  ]
}
```

都可以不写。`tone` 是记号的颜色:`ok` / `warn` / `bad` / `dim`。模板里的 `{id}`、`{text}` 是行自己的,别的在行的
`fields` 里,找不到再看列表的 `fields`。`aca plugin view <插件.视图>` 把列表打成一行一条(`--json` 原样打)。

## 交给命令的环境

| 变量 | |
|---|---|
| `ACA_BIN` | 现在这个 aca 的完整路径。调 aca 都用它 |
| `ACA_ENV` | `1` |
| `ACA_PLUGIN_ID` / `ACA_PLUGIN_ROOT` | 插件 id、插件目录 |
| `ACA_PLUGIN_CONFIG_DIR` | 用户可改的配置放这(`~/.config/aca/plugins/<id>/`) |
| `ACA_PLUGIN_STATE_DIR` | 插件自己的状态放这(`~/.local/state/aca/plugins/<id>/`) |
| `ACA_PLUGIN_CONTEXT_JSON` | `{"session_id", "session_dir", "session_pane", "session_parent", "invocation_source"}` |
| `ACA_PLUGIN_SETTINGS_JSON` | 全部设置(清单的默认值,plugins.toml 里改过的盖上去) |
| `ACA_SETTING_<名字>` | 一个设置一个(名字大写,`-` `:` 换成 `_`)。字符串原样,别的照 TOML 的写法:`true`、`5`、`["a", "b"]` |
| `ACA_SESSION_ID` / `ACA_SESSION_DIR` | 冲着哪个会话、它的目录(没有会话时设成空的,不是不设) |
| `ACA_SESSION_PANE` | 那个会话里 agent 的那一格(`%5`):界面、命令行冲着一个在跑的会话时有,钩子跑在那个会话自己的 hook 里时有;agent 调的工具里是空的 |
| `ACA_SESSION_PARENT` | 派它的那个会话(在某个会话里 `aca new` 出来的都挂在它下面);自己开的是空的 |
| `ACA_PLUGIN_ENTRYPOINT_ID` | 这一回跑的是哪个:能力、视图、窗格的 id(能力另外还有一份老名字 `ACA_PLUGIN_ACTION_ID`) |
| `ACA_PLUGIN_ARGS_JSON` | 能力:给的参数(照 `args` 的类型;没给参数是 `{}`) |
| `ACA_ARG_<名字>` | 给了的参数再一个一个放一份(名字大写,`-` `:` 换成 `_`;没给的可选参数不设):shell 写的插件不用解 JSON。字符串原样,整数、布尔是 `3`、`true` |
| `ACA_PLUGIN_EVENT` / `ACA_PLUGIN_EVENT_JSON` | 事件钩子:事件名、整行事件 |
| `ACA_PLUGIN_TIMER` | 定时跑的能力:是哪个定时(`插件.能力`);别的时候是空的 |
| `ACA_INPUT_FILE` | 窗格写了 `input` 的:会话的内容在这个文件里(命令、视图写了 `input` 的从标准输入读)。`reply` 是 hook 记下的最后一条回复;没记到(接管来还没重启的会话)或者太长被截了,aca 去它的对话记录里读全的 |
| `ACA_INVOKED_BY` | 谁调的:`menu` / `cli` / `agent` / `event` / `timer` |
| `ACA_LANG` | 界面现在用哪种话:`zh` / `en`。插件说的话照它 |

两个目录 aca 只管建,里面的文件、格式、清理都归插件(会话删了要清的,订 `session.removed`)。
命令还继承 aca 自己的环境(`PATH` 这些),表里的变量盖在上面。

## 事件

一行一个 JSON,公共字段 `ts`(毫秒)、`event`、`session`。

| 事件 | 自己的字段 | 什么时候 |
|---|---|---|
| `session.created` | `title` `tool` `account` `dir` `group` `parent` | 建了(分叉出来的多 `fork` 和分自哪个会话 `fork_of`,`parent` 是空的:没人派它;`aca adopt` 接管来的多 `adopted` 和现在的会话名 `tmux`) |
| `session.started` | `how`:fresh / resume / fork | agent 起来了 |
| `session.stopped` | | `aca stop` |
| `session.renamed` | `title` `tmux` | 人改的名 |
| `session.titled` | `title` `tmux` | agent 自己起了 / 换了标题 |
| `session.moved` | `group` | 挪分组 |
| `session.rehomed` | `tool` `account` `from`(原来的「工具:账号」) | 换账号接着跑(`aca restart --as`、界面 `H` 里挑同一家的别的账号):对话拷到新账号,在原地重起 |
| `session.archived` / `session.removed` | 删的带 `title`;`aca release` 放手的多一个 `released`(tmux 会话还在) | |
| `turn.started` | `prompt` | 你发了一句 |
| `turn.done` | `last`(最后一条回复)`bg`(还挂着的后台任务数)`secs` | 一轮跑完 |
| `turn.failed` | `error` `kind`(transient = 限流、过载、断流,过一会儿接着说多半就好;account = 额度、登录、欠费;fatal = 再试也一样) | 一轮失败(Codex 不报这个 hook:aca 看状态时从它的对话记录里读出来补上) |
| `turn.interrupted` | `by`(`aca interrupt` 中止 Claude 时:谁按的) | 你按 Esc 打断了(Codex 自己报);或者 `aca interrupt` 中止了它 |
| `agent.waiting` | `ask` `kind`(choice / permission / 空 = 看不出)`options`(选择题的选项) | 在等你确认 / 回答 |
| `agent.exited` | `reason` | agent 进程退出 |
| `conversation.changed` | `conv` `from` | 对话 id 换了(/clear) |
| `message.sent` | `id`(这条消息的)`from` `kind`(msg / notice)`what`(通知说的事;不是通知是 null)`wake` `text` | 往这个会话的收件箱放了一条(`aca msg`,或者它派出去的 worker 自动发的通知) |
| `message.delivered` | `id` `how`(bell / queue / type / wait) | 那条送到了它眼前 |
| `message.throttled` | `from` `count` `secs` | 同一个会话发得太密,后面的只存不叫醒 |
| `ask.answered` | `by`(谁替它答的;`you` = 在外面)`answer` `ask` `kind`(choice / permission;派活的关卡是 `gate`) | `aca answer` 替它回答了确认框 |

全部事件在 `~/.local/state/aca/events.jsonl`;`aca events -f` 一直读。

## 定时

`[[timers]]` 每隔 `every` 跑这个插件的一个能力。aca 没有常驻进程,到点靠 macOS 的 launchd:开着的定时一个一个
LaunchAgent(`~/Library/LaunchAgents/com.aca.timer.<插件>.<能力>.plist`),到点跑 `aca plugin timer tick <插件.能力>`。
合盖睡着错过的几次,醒来只补跑一次。

- **开关**:没写 `manual` 的,插件开着就开着(链接、装、更新、开关、删插件时 aca 照清单对一遍)。`manual = true` 的平时关着,
  插件自己开关:有活了 `"$ACA_BIN" plugin timer on "$ACA_PLUGIN_ID.collect"`,做完了 `off` —— 没活的日子里什么都不跑。
  插件停着时都不跑,开回来照原来的开关。
- **跑的时候**:跟命令行跑这个能力一样的环境,`ACA_INVOKED_BY=timer`、`ACA_PLUGIN_TIMER=<插件.能力>`,没有会话(`ACA_SESSION_ID` 是空的),
  标准输入是空的。输出进插件的日志(`aca plugin log`),最多 10 分钟(到了 TERM 它那一组,3 秒不退 KILL);上一次还没完,下一次不起。
- **跑着的那一次里开关它自己**(比如做完了 `off`):aca 等这一次跑完再动 launchd,不会把自己半路收走。
- PATH 照开它的那一刻抄进 plist(launchd 自己的 PATH 里没有 Homebrew 的东西)。
- 测试、基准(设了 `ACA_HOME` 或 `ACA_TMUX_SOCKET`)不碰 launchd:plist 写在 aca 的状态目录 `timers/` 里当作装上了。

```sh
aca plugin timer                       # 每个定时:● 开着 ○ 关着、每隔多久、上次跑得怎么样;--json 一行一个
aca plugin timer on | off <插件.能力>   # 开关(没写 manual 的跟着插件开关,off 不了)
aca plugin timer run <插件.能力>        # 现在跑一次,像到点了一样:输出打出来,关着也跑
```

`--json` 一行一个:`{"id", "every"(秒), "manual", "on"(要它开着), "enabled"(插件开着), "scheduled"(launchd 那头装着),
"last"(上次开始的时刻,0 = 没跑过), "secs", "rc"(退出码;跑超了停掉 / 起不来是 null), "why"}`。

## 出错了

钩子和定时在后台跑,没人看着:上次没跑成(退出码不是 0、起不来、跑超了),或者 gateway 下面的服务连着失败 5 次停下了,
aca 点名是哪个插件:

- tmux 状态栏那一段(`aca tmux-status`)后面多 `⚠ bark`(aca 自带的去掉 `aca.`;最多两个名字,多的写 +N);
- 界面顶栏右边一格 `⚠ 1 插件出错(P)`,插件页那一行标「出错了」,右边写哪一处、怎么了、几时;
- `aca plugin ls` 一处一行:`⚠ me.hello:钩子 turn.done 退出码 3 · 5m 前(aca plugin log me.hello)`。

钩子是不等的:aca 起它时外面包一层 sh,跑完把退出码记进 `~/.local/state/aca/plugin-health/<插件>/hook-<第几个>`。
只算开着的插件、一天以内的;同一处下一次跑成了就不再提,插件停了、删了不提,重新链接 / 更新(清单可能换了,第几个对不上)记录清掉。
所以退出码就是插件说「这次没办成」的方式:没办成别 `exit 0`;只是没活可干,`exit 0`。

## 插件常用的命令

```sh
"$ACA_BIN" show "$ACA_SESSION_ID" --json     # 标题、状态、目录、对话 id、最后一条回复、有没有人连着
"$ACA_BIN" ls --json --no-screen             # 全部会话
"$ACA_BIN" send <id> -m '…' --wait 600       # 等它空下来再往输入框发一段话
"$ACA_BIN" meta set <id> <名字> <值> [--ttl 秒]   # 在会话行上挂一小段字
"$ACA_BIN" meta rm <id> <名字>
"$ACA_BIN" msg <id> < 话.txt                  # 放进它的收件箱,挑它方便的时候送进去(在跑的等这一轮跑完)
"$ACA_BIN" plugin timer on | off "$ACA_PLUGIN_ID.<能力>"   # 开关自己的定时(manual 的)
"$ACA_BIN" new / start / stop / fork / rename / move / rm
```

`meta set` 是插件往列表里放东西的办法:名字 1–32 个字母数字 `_` `-`,值截到 80 个字,`--ttl` 到期自己消失。
`aca ls` 里显示成 `[名字 值]`,`--json` 里在 `tokens`。

## 接口约定(api 1)

清单里的 `api = 1` 说插件照这一版写(不写也算 1;比这个 aca 新的读不进来,链接时说先升级 aca)。同一版里 aca 只加不改:

- **会加**:新的可选字段、新的环境变量、新的事件、事件和 JSON 里新的字段。读 JSON 时不认识的字段跳过,别因为多了一个就报错。
- **不会变**:下面这些的名字、意思、类型。要删、要改名就是 `api = 2`。

| 稳定的 | 写在 |
|---|---|
| 清单的字段 | 「清单」 |
| 交给命令的环境变量 | 「交给命令的环境」 |
| 定时、`aca plugin timer --json` | 「定时」 |
| 事件和它们的字段 | 「事件」 |
| 列表视图的 JSON | 「列表视图的输出」 |
| 给 agent 的工具:名字、参数、返回 | 「能力给 agent 当工具」和下面 |
| `aca ls --json` / `aca show --json` 外面那一层、`aca plugin actions --json` | 下面 |
| 输出、退出码、超时 | 下面 |

**输出、退出码、超时**

- 退出码 0 = 成了,别的都算没成,没有别的意思。
- 标准输出是结果。标准错误:命令行、agent 的工具、界面里的能力和视图,接在标准输出后面一起给人看;**列表视图跑成了只读标准输出**
  (那是 JSON),标准错误记进插件的日志(`aca plugin log`),没跑成两样都给人看。
- 界面里跑的、`aca plugin view` 最多 120 秒,agent 的工具 280 秒,定时 10 分钟:到时间先 TERM 它那一组进程,3 秒还不退就 KILL。
  `aca plugin run`、事件钩子、窗格不限时。
- 命令自己退出了,留在后台的进程还拿着它的输出:aca 收下已经读到的就走,不跟着等。要在后台常驻的,把输出接到别处。
- 标准输入:命令行跑的接着你的终端;界面、agent 的工具、钩子、定时是空的;写了 `input` 的从这里读会话的内容。

**aca 打出来的 JSON**

- `aca ls --json`(数组)、`aca show <id> --json`(一个)稳定的字段:`id` `title` `state`(running 在跑 / ask 在等你 /
  unread 跑完了没看 / idle / error / exited agent 退出了 / stopped 没在跑)`tool` `account` `group` `dir` `parent`(谁派的)
  `fork_of`(分叉出来的:分自哪个会话;它没有 `parent`)
  `tmux`(tmux 会话名)`pane`(agent 那一格)`attached`(几个终端连着)`conv`(对话 id)`ask`(在等你时问的那句)
  `ran`(这一轮跑了几秒)`active`(最近有动静的时刻)`bg`(后台任务数)`locked` `created` `order` `tokens`(`meta set` 挂的字)。
- `aca show --json` 里的 `run` 是 aca 自己记的状态,**不是接口**,里面的字段随时会变。
- `aca plugin actions --json`:一行一个能力,`{"id", "title", "contexts", "args", "expose"}`。
- 时间:事件的 `ts` 是毫秒,别处是秒。JSON 是紧凑的一行,用 `jq` 读,别拿 sed 去拆(怎么排版不算接口)。

**名字**

- 给 agent 的工具名是 `<插件 id,点换成 ->__<能力 id>`。所以给 agent 用的能力 id 里不能有 `:` 和 `__`;
  两个插件的 id 换出来一样(`a.b` 和 `a-b`)不能同时链接。
- 参数名、设置名换成环境变量时大写、`-` `:` 换成 `_`:换出来一样的两个不能写在一起。
- 能力、窗格、视图共用一套 id(`[[keys]]`、命令行都只拿 id 找)。
- 老写法照认,文档只写新的:`[[actions]]` = `[[commands]]`,`ACA_PLUGIN_ACTION_ID` = 能力的 `ACA_PLUGIN_ENTRYPOINT_ID`,
  `[agents.claude] plugin_dir` = `[agent] dir`。

## 第一方插件

在仓库的 [`plugins/`](../plugins) 里,编进 aca 一起发,都默认不装。`aca plugin install aca.<名字>` 不用写路径;
plugins.toml 里写一个光秃秃的 `[plugins."aca.core"]`(不写 source)换台机器 `sync` 也装得上。
装 = aca 把编进来的文件写到 `~/.local/share/aca/first-party/<名字>/` 再登记;那份归 aca,升级 aca 后内容变了,
下次起 agent、`sync`、开插件页时照新的重写(插件页、`aca plugin ls` 里标「自带」)。

| 插件 | 给什么 |
|---|---|
| `aca.delegate` | `/aca:delegate`(Claude 是斜杠命令,敲 `/dele` 补全就是它;Codex 是一个 skill):领头的规划、经 aca 派 worker、验收、汇报;worker 临时失败自动续。设置:`worker`(派给谁,默认 `codex`)、`language`(回你用哪种话,空 = 跟着你)、`say`(替它说的那句)、`max_continue`、`delay` |
| `aca.core` | 把 aca 的核心能力给 agent 当工具(两家都行):`sessions` 看会话、`dispatch` 派 worker(挂在调用者下面,可以带 worktree 分支)、`message` 递话、`wait` 等(最多 240 秒一次)、`answer` 替它回答、`interrupt` 中止。替谁回答、中止谁,只能是调用者自己派出去的 |
| `aca.gateway` | MCP gateway,见下一节 |
| `aca.remote` | 远端任务,整个在插件的脚本里(sh + jq + curl + ssh):把长任务派到别的机器上后台跑,结束时结果作为一条消息放进派它的会话的收件箱。`:` 菜单里 `r` 派、`t` 列表(⏎ 复制看日志的命令、`x` 撤掉、`r` 现在去取)、「测一下 server」;agent 有 `run`、`list` 两个工具。回调先落在一台一直在线的 server 上([jobhook](https://github.com/cheny-00/jobhook)),设置:`url`、`token`。有在等的才开着每 5 分钟去取的定时 |
| `aca.tailnet` | tailnet 挂载,整个在插件的脚本里(sh + jq,要 `jq`):`:` 菜单里 `m` 是列表,一行一条 `tailscale serve`(目标在不在听),⏎ 复制地址、`x` 撤掉(写出怎么加回去)、`a` 把本机端口挂出去、`p` 开关发布(装了 tpub)。agent 能列;挂端口是 `agent?`,你点了名才给(`aca plugin allow aca.tailnet add`) |
| `aca.bark` | 跑完了、在等你确认、出错了用 Bark 推到手机(有人正连着那个会话就不推)。设置:`url`(推送地址)、`workers`(派出去的 worker 也推)、`group`;`: 菜单`里「推一条试试」 |
| `aca.discord` | 同样三件事发到 Discord 频道(webhook;有人正连着那个会话就不发)。设置:`url`(webhook 地址)、`mention`(在等你确认时 @ 的用户 id)、`workers`、`username`。只往外发;在 Discord 里回话发进会话还没做 |
| `aca.notify` | 同样三件事在这台机器上弹桌面通知(你正连着那个会话就不弹)。设置:`command`(换一个弹通知的命令,标题、正文是它的两个参数;空着或者它发不出去时 macOS 用 `osascript`、Linux 用 `notify-send`)、`workers`、`attached`(正连着时也弹);`: 菜单`里「弹一条试试」 |
| `aca.copy` | 复制回复里的命令,整个在插件的脚本里(sh,要 `fzf`):在会话里按 Ctrl+Alt+Y(热键;`:` 菜单里是 `y`),从最后一条回复的原文里认出代码块和行内的命令 —— 只有一条直接复制,多了弹出来按数字键挑;shell 代码块里的每一条也单独列着。复制到 tmux 的缓冲区、外层终端(OSC 52)和本机剪贴板。设置:`inline`(行内的 `…` 要哪些)、`lines`、`copy`(换一个复制的命令) |

## MCP gateway

第一方插件 [`plugins/gateway`](../plugins/gateway)(`aca.gateway`)是 MCP 的注册中心。装上、开着时:

- agent 身上只挂一个 MCP 服务 `aca`:一个很薄的转接(`aca plugin gateway connect`),stdio 进,本机 Unix socket 出,连到 gateway。
- gateway 下面挂着:插件的 MCP(`[agent.mcp]`,`via` 不是 `direct` 的)、插件露给 agent 的能力、config.toml 里 `[mcps.<名字>]`
  在 gateway 的设置 `servers` 里点了名的。工具名是 `<服务>__<工具>`(能力的工具本来就是 `<插件>__<能力>`,不再加)。
- 每个服务第一次有人要时才起。`scope` 是 `shared`(默认)的只起一份,所有会话共用;`session` 的每个会话一份,
  这个会话断了就收掉(有状态的用这个,比如一个浏览器)。连着失败退避重启,连败 5 次停下。
- 谁在调:转接连上先报会话;gateway 转调用时在 `_meta` 里加 `dev.aca/session`。共用的插件 MCP 从这读调用者
  (每会话一份的环境里还有 `ACA_SESSION_ID`)。每个会话看到的工具按 plugins.toml 的 `agents` 算。
- gateway 是按需起的进程:第一个转接发现它没在跑就拉起来(加锁,只起一个),没人连着过 `idle_minutes` 分钟自己退出。
  状态在 `~/.local/state/aca/gateway/`:日志一个服务一份(`logs/`),每次调用一行(`calls.jsonl`)。

```toml
[plugins."aca.gateway"]
source = "~/src/aca/plugins/gateway"
settings = { servers = ["web", "playwright"], idle_minutes = 10 }
```

```sh
aca plugin gateway status            # 在不在跑、谁连着、每个服务的样子、最近的调用
aca plugin gateway restart [服务]     # 重启下面一个服务(下次有人要时再起)
aca plugin gateway log <服务>         # 它的 stderr
aca plugin gateway stop
aca plugin gateway add | drop <名字>  # 把 config.toml 的 [mcps.<名字>] 收进 / 移出 gateway
aca plugin gateway off | on <服务> [--session id]   # 只对一个会话关掉 / 打开一个服务
aca plugin gateway import [--apply]  # 各账号里散装的 MCP 列出来;--apply 收进 gateway
```

- **界面**:插件页(`P`)里 gateway 那一行按 `m` 进去,一行一个服务(在跑 / 没起 / 崩了、几个工具、一天调了几次、几个会话在用),
  空格把 `[mcps.*]` 收进 / 移出,`r` 重启、`l` 看日志、`s` 停掉。`M` 页最上面是这个会话从 gateway 看得到的服务,
  空格只对这个会话关掉 / 打开(Claude 马上换,Codex 重启后);收进了 gateway 的 `[mcps.*]` 不再在「项目」里重复给。
- **把账号里的 MCP 收进来**:`import` 读 Claude 账号目录里 `.claude.json` 顶层的 `mcpServers` 和 Codex 账号目录里
  `config.toml` 的 `[mcp_servers]`,同名同写法的并成一个,HTTP 的(gateway 现在只接 stdio)、几处写得不一样的标出来不收。
  `--apply` 写进 aca 的 config.toml `[mcps.<名字>]`(已经有的不动)再收进 gateway。账号里的那份原样留着:
  gateway 开着时 agent 会两份都看到,确认没问题后自己去账号里删。

停了这个插件:各自照旧直接挂到 agent 上(正开着的会话重启后变)。

## plugins.toml:要哪些插件

`~/.config/aca/plugins.toml` 列着要哪些插件,像 vim-plug 列在 vimrc 里。可以跟 dotfiles 一起放,换台机器 `aca plugin sync` 一条。

```toml
[plugins."aca.delegate"]
source = "~/src/aca/plugins/delegate"   # 本地目录 = 链接;git 地址 / owner/repo = 克隆
agents = ["claude:*", "codex:work"]    # 给哪几个「工具[:账号]」带东西;不写 = 清单里给的那几家全给
settings = { max_continue = 3 }        # 盖过清单里的默认值
tools = ["dispatch"]                   # 给 agent 哪些工具(见「你说了算」)
keys = { tasks = "T" }                 # : 菜单里的键

[plugins."owner/gh-glance"]
source = "owner/gh-glance"
ref = "v1.2"                           # 钉在一个标签 / 分支 / 提交上
enabled = false                        # 先停着
```

- 命令行和界面(`P`)里的装、删、开停、改设置都顺手改这份文件:只改那一个插件的那几行,别的、注释原样留着。
- 你手改了它:`aca plugin sync` 让本机跟它一致 —— 缺的装、没写的删、开关照它。界面里 `P` 页按 `e` 打开它
  (停在高亮那个插件那一段),改完回来自己同步;别处改的(dotfiles 拉下来的)插件页底下写出跟本机差在哪,`S` 同步。
- `plugins.lock` 记 git 装的每个插件是哪个提交:`sync` 照它装(换机器装出来一样),`update` 拉新的后改它。
- 设置、`agents` 改了不用 sync:下一次跑命令、起 agent 时就是新的。
- 还没有这份文件时,第一次用到就照现在登记的插件写一份。

## 装和调试

```sh
aca plugin install aca.delegate      # aca 自带的,不用写路径;装完列出它带了什么
aca plugin install owner/repo        # GitHub 上的;也可以是任何 git 地址。克隆到 ~/.local/share/aca/plugins/<id>
aca plugin install <git 地址> --ref v1.2   # 钉在一个版本上(update 不动它)
aca plugin update [id]               # git 装的拉新的(只快进);不给 id = 全部。新的提交记进 plugins.lock
aca plugin sync                      # 让本机跟 plugins.toml 一致
aca plugin remove <id>               # 不再用:git 装的连目录删,链接的不动;它的热键解掉。插件自己的配置、状态留着(空目录不留)
aca plugin new ./my-plugin           # 建一个新插件(清单 + 脚本);--id 给 id,不给是 me.<目录名>
aca plugin check [目录 | id]          # 查:清单(不认识的字段也点名)、要跑的程序和脚本、needs;有毛病退出码 1
aca plugin link ./my-plugin          # 链接本地目录(你正在写的;改了清单再 link 一次)。install 一个本地目录也是 link
aca plugin ls                        # ● 开着  ○ 关着;从哪来的;给哪家 agent 带了东西;哪些开着的会话要重启才带上
aca plugin actions                   # 能力、窗格、视图
aca plugin run example.git-glance.refresh top=5 --session 3f2a   # 参数写成 名字=值
aca plugin view aca.remote.tasks     # 跑一个视图,打出输出
aca plugin setting aca.delegate [worker [codex:work]]   # 列设置 / 读一个 / 改一个(写进 plugins.toml);--reset 回默认
aca plugin allow | deny <id> [能力…]    # 给 agent 哪些工具(见「你说了算」)
aca plugin key [<id> <目标> <键>]       # : 菜单里的键
aca plugin pane example.git-glance diff --session 3f2a [--placement window]
aca plugin log example.git-glance    # 事件钩子的输出;列表视图的标准错误也记在这
aca plugin emit turn.failed kind=transient [--session id] [--plugin id]   # 假装来了一个事件,钩子在前台跑
aca plugin timer run aca.remote.collect   # 现在跑一次定时,像到点了一样,输出打出来
aca plugin disable / enable / unlink <id>
```

事件钩子是 hook 进程里起的,**不等它跑完**,输出进 `plugin-logs/<id>.log`。动作在前台跑,输出直接给调用的人。
窗格:`popup` 开在 tmux 弹窗里(人不在 tmux 里时直接在当前终端前台跑),`split` 开在 agent 那格右边,`window` 是那个 tmux 会话里的新窗口。

例子在 [`plugins/examples/`](../plugins/examples):`git-glance`(事件 + 动作 + 窗格 + 行上的字)。第一方插件也是现成的例子,
`aca.tailnet`、`aca.remote` 最完整(只用插件接口,不靠 aca 里的代码;remote 还用了定时)。

起 agent 时插件给了什么,aca 记在会话的运行时状态里(`mounts`);之后装、删、开停插件或改了 `agents`,
`aca plugin ls` 和插件页会点名哪些开着的会话起来时带的和现在不一样:重启它们才带上。

[`plugins/delegate/`](../plugins/delegate)(`aca.delegate`)是一个能直接用的:给 Claude 带一个 `/aca:delegate` 命令
(敲 `/delegate` 补全就是它),给 Codex 一个叫 delegate 的 skill —— 领头的规划、用 `aca new -w` 把活派给 worker
(设置 `worker`,默认 `codex`;各在自己的 worktree 里)、`aca wait` 等它(Claude 放后台,Codex 在前台 `--any`)、
自己验收、汇报;派出去的 worker 一轮因为限流 / 过载失败时,它的事件钩子过一阵替它说一句(设置 `say`,
最多 `max_continue` 次,间隔 `delay`)。装:`aca plugin install aca.delegate`。

## 和 herdr 插件的区别

| herdr | aca |
|---|---|
| `herdr-plugin.toml` | `aca-plugin.toml`,动作 / 事件 / 窗格三段的写法一样;多了能力的参数、设置、事件的 `if`、给 agent 的部分 |
| `HERDR_*` 环境变量 | `ACA_*`,一一对应 |
| 常驻的 server,socket API,`events.subscribe` | 没有常驻进程:事件是一个追加写的文件,`aca events -f` 读 |
| `pane.report_metadata` 的令牌 | `aca meta set` |
| 窗格 placement:overlay / popup / split / tab / zoomed | popup / split / window(都是 tmux 自己的) |
| `[[startup]]` | 没有:没有 server 要等。要隔一阵跑一次的写 `[[timers]]`(aca 替它装 launchd) |
| `[[keybindings]]` | `[[keys]]`:绑在界面 `:` 菜单里(`:` 再按那个键),不和 aca 自己的键抢;只是默认值,你在 plugins.toml 里改 |
| | `[[views]]`:命令的输出放进 aca 的全屏阅读页,插件不用自己写界面 |
| `plugin install <GitHub>` | `aca plugin install owner/repo`(任何 git 地址都行,`--ref` 钉版本),`update`、`remove` |
| | `[agent]`:给 aca 起的 agent 带一个插件目录(skill、斜杠命令)和一个 MCP 服务 |
| | `plugins.toml` + `plugins.lock` + `aca plugin sync`:声明式地管;给 agent 的工具、键也在这里由你定 |
| | `input`:把会话的最后一条回复 / 屏幕交给插件(kitty 的 kitten 读窗口内容那样) |
| | `~/.config/aca/user/`:不建插件也能加小东西 |
| `[[link_handlers]]`、`[[build]]` | 还没做 |
