# 编写 aca 插件

[English](plugins.md) | 简体中文

插件是一个目录:一份 `aca-plugin.toml` 清单,加上它要运行的命令。用什么语言写都可以,没有 SDK。
清单声明插件提供什么,aca 负责在合适的时机运行这些命令、把给 agent 的内容带进去;插件要做什么,就回头调用 `aca` 命令行 ——
**命令行就是插件的 API**。

插件可以从两个方向扩展:扩展 aca 本身(界面里的键、菜单、窗格,命令行,事件钩子,定时任务),或者扩展 agent(skill、斜杠命令、MCP 工具)。

设计参考了 [herdr](https://github.com/herdrdev/herdr) 的插件,区别见最后一节。

插件声明它想要什么,最终由你决定(像 kitty:kitten 不自己绑定键,远程控制的每个口令能用哪些命令由你列出):给 agent 哪些工具、
`:` 菜单里用哪些键,都在 `plugins.toml` 里修改(见「由你决定」)。不想建插件的话,写一份 `~/.config/aca/user/aca-plugin.toml`
即可生效(见「你自己的插件」)。

## 写第一个插件

```sh
aca plugin new ./hello            # 创建清单和脚本:一条命令、一个键、一个设置,另有一段注释掉的事件钩子
aca plugin check ./hello          # 不链接也能检查:清单是否正确、要运行的程序 / 脚本是否存在、needs 里的程序是否已安装
aca plugin link ./hello           # 链接:修改脚本后立即生效,修改清单后需重新 link
aca plugin run me.hello.hello     # 运行一次(界面中:在会话列表按 : 再按 h)
aca plugin emit turn.done --plugin me.hello   # 模拟一个事件:钩子在前台运行、直接输出,无需真的运行 agent
aca plugin log me.hello           # 平时钩子的输出在这里
```

写好后推到 GitHub,别人运行 `aca plugin install 你/仓库` 就能安装。

可以照着改的例子:[`plugins/tailnet/`](../plugins/tailnet)(`aca.tailnet`)的全部功能都在一个 sh + jq 脚本里 ——
列表视图、带参数的命令、给 agent 的工具、修改后重新读取核对、中英文,aca 本身没有一行挂载相关的代码。
[`plugins/remote/`](../plugins/remote)(`aca.remote`)也是这样:设置(含口令)、定时任务(有任务时才开启)、往会话的收件箱发消息。
小一些的:[`plugins/examples/git-glance/`](../plugins/examples/git-glance)(事件 + 动作 + 窗格 + 会话行上的文字)。

## 清单

```toml
id = "example.git-glance"        # 字母、数字、. : _ -
name = "Git Glance"
version = "0.1.0"
api = 1                          # 按插件接口的第几版编写(见「接口约定」);不写 = 1。比当前 aca 支持的版本新时无法读取
min_aca_version = "0.1.0"        # 可选;比当前 aca 的版本新时不允许链接
description = "每轮结束后,在会话行上显示改动统计"
needs = ["git"]                  # 可选:依赖的外部程序(在 PATH 中查找,"a|b" = 有其一即可)。aca 不代为安装,
                                 # 安装时、aca plugin check、aca plugin ls 和插件页会指出这台机器缺少哪个
requires = ["aca.core"]          # 可选:依赖的插件,只能是 aca 自带的第一方插件,安装时一并安装
                                 # (以上字段要写在第一个 [表头] 之前:TOML 中写在表头之后的字段都属于那张表)

[settings.max_lines]             # 设置:默认值和说明;值保存在 plugins.toml,运行命令时通过环境变量传入(ACA_SETTING_MAX_LINES)
default = 20                     # 值按默认值的类型读取:在界面或 aca plugin setting 里填 "20x" 会提示应为整数
desc = "改动统计最多显示几行"
# secret = true                  # webhook 地址、key 这类:界面和 aca plugin setting 列出时打码
                                 # (值仍以明文保存在 plugins.toml 中,所以 aca 把这个文件的权限设为仅你可读的 0600)

[[events]]                       # 事件钩子:某件事发生时运行一条命令,不等待它结束
on = "turn.done"                 # 也可以写 "turn.*"、"*"
if = { parent = true }           # 可选:条件不符时不启动进程(见下面「事件钩子的 if」)
command = ["sh", "stat.sh"]

[[commands]]                     # 能力:一条命令(旧写法 [[actions]] 仍然可用)
id = "refresh"                   # 字母、数字、: _ -(不能有点)
title = "刷新改动统计"
desc = "重新计算这个会话的改动统计"   # 给 agent 看的说明;不写则用 title
contexts = ["session"]           # session / group / global:在哪种对象上可用
args = { path = "path?", top = "int?" }   # 参数:string / path / int / bool,带 ? 的可以不给
expose = ["menu", "cli"]         # 提供给谁:menu(界面菜单、键)、cli(aca plugin run)、agent(作为 agent 的工具)、
                                 # agent?(可以提供给 agent,但默认不提供:会改动机器的写这个,需你明确允许)。不写 = menu + cli
input = "reply"                  # 可选:把会话的哪些内容交给它 —— reply 最后一条回复 / screen 屏幕上当前的文字 / scrollback 包括回滚内容
                                 # (命令、视图从标准输入读取;窗格读取 ACA_INPUT_FILE 指向的文件)
cwd = "session"                  # 可选:在会话的目录里运行(lazygit 这类);不写 = 插件目录。./x 仍按插件目录查找
command = ["sh", "stat.sh"]

[[commands]]
id = "close"                     # 下面列表视图的键运行的就是它
title = "关闭 PR"
args = { number = "string" }
expose = ["cli"]
command = ["sh", "close.sh"]

[[commands]]
id = "fetch"                     # 下面的定时任务运行的就是它
title = "拉取远端"
expose = ["cli"]
command = ["sh", "fetch.sh"]

[[timers]]                       # 定时任务:每隔一段时间运行这个插件的一个能力(见下面「定时」)
run = "fetch"                    # 能力的 id:不能有必填参数,也不能只在会话上可用(到点运行时没有会话)
every = "30m"                    # 写法:30s / 10m / 2h / 1d,最短 1m
# manual = true                  # 平时关闭,由插件自己运行 aca plugin timer on / off(有任务时才开启);不写 = 随插件开启

[[panes]]                        # 窗格:一个终端程序
id = "diff"
title = "改动"
contexts = ["session"]
placement = "popup"              # popup(默认)/ split / window
width = "90%"                    # 弹窗的大小:格数或百分比
height = "90%"
command = ["sh", "diff.sh"]

[[views]]                        # 视图:一条命令的输出,显示在 aca 的全屏阅读页(可以翻页、复制;适合在手机上看)
id = "ci"
title = "CI 状态"
contexts = ["session"]
command = ["sh", "ci.sh"]

[[views]]                        # 列表视图:命令输出一份 JSON(格式见「列表视图的输出」),每条一行,每行可以按 keys 里的键
id = "prs"
title = "PR"
rows = true
command = ["sh", "prs.sh"]

[[views.keys]]
key = "enter"                    # 一个字符,或者 enter;j k q 留给界面
title = "复制地址"
copy = "{url}"                   # 复制:模板里的 {名字} 替换成这一行的字段

[[views.keys]]
key = "x"
title = "关闭"
run = "close"                    # 带着这一行运行这个插件的一个能力(是否提供给菜单都可以)
args = { number = "{id}" }       # 参数从这一行填入;能力需要的其他参数先弹出表单询问
confirm = true                   # 需要按两次
# row = false                    # 不需要选中某一行(比如新建一条);模板读取列表的 fields

[[keys]]                         # 界面 : 菜单里按这个键直接运行(能力 / 窗格 / 视图的 id)
key = "c"
run = "ci"

[[hotkeys]]                      # 热键:在会话里面不按前缀直接按(往上翻看时也可以),打开一个窗格。见「热键」
key = "C-M-g"                    # tmux 的键名,要带 Ctrl / Alt(C-]、M-y、C-M-g),或者 F1–F12
run = "diff"                     # 窗格的 id(热键只能打开窗格)

[agent]                          # 带给 agent 的内容
for = ["claude", "codex"]        # 给哪几种 agent;不写 = 全部
dir = "agent"                    # 一个 Claude 格式的插件目录(skill、斜杠命令),相对于插件目录
# auto = "auto"                  # 一个开关设置的名字:开启时 agent 也能自行使用 dir 里的命令(见下文)

[agent.mcp]                      # 一个 MCP 服务(stdio)
command = ["./mcp-server"]       # 相对于插件目录
via = "auto"                     # auto:gateway 开启时登记到 gateway,否则直接挂到 agent 上;direct / gateway 强制指定
scope = "shared"                 # 在 gateway 中:shared 所有会话共用一份 / session 每个会话一份(有状态的服务)
```

`command` 是参数数组,不经过 shell,工作目录是插件目录。

清单里无法识别的字段不会阻止链接,但会被指出来(链接时、`aca plugin check`、插件页):多半是拼错了,或者写到了别的表下面
—— TOML 中写在 `[[keys]]` 这类表头之后的字段都属于那一条。

**两种语言**:`name`、`description`,以及各处的 `title`、`desc`,都可以按语言另写一份:`description_en = "…"`、
`title_zh = "…"`。界面是对应语言时用带后缀的那份,其他情况用不带后缀的那份(所以不带后缀的必须写)。旧版 aca 不识别后缀,
仍然使用不带后缀的那份。aca 自带的插件都把中文写在不带后缀的字段里、英文写在 `_en` 里。

`[agent] dir`:aca 启动 agent 时带上这个目录,只对那一次启动有效,不安装到它的配置目录,也不改动你的设置。
插件关闭或删除后,agent 下次启动时就没有了(正在运行的要重启)。这个目录是一个普通的 Claude Code 插件
(`.claude-plugin/plugin.json` 加 `commands/`、`skills/` ……)。命令文件开头写了 `disable-model-invocation: true` 的,
只在你输入那个斜杠命令时才进入上下文。旧写法 `[agents.claude] plugin_dir = "claude"` 仍然可用(只给 Claude)。

`[agent] auto`:想让用户自己决定「agent 能否自行使用这些命令」时写。它指向 `[settings]` 里一个默认值为 `true` / `false` 的设置
(在插件页按 ⏎ 切换,或者运行 `aca plugin setting <id> <名字> on|off`)。开启时:Claude 拿到的是 aca 生成的一份副本,命令和 skill 开头的
`disable-model-invocation` 已去掉,它们的描述会进入 Claude 的列表,你说一句它就会用;关闭时:按文件里写的来。不写 `auto` = 始终按文件里写的来。
只对 Claude 有效(Codex 的 skill 本来就都在它的列表里),修改后对新启动、重启的会话生效。`aca.delegate` 的 `auto` 设置就是这样用的。

### 怎么带进 agent(挂载)

全部在启动时带入,只对那一次有效;不修改账号的配置文件,不往你的仓库里写东西。没有已开启的插件给这个会话提供内容时,命令行不会多出任何参数。

| | Claude | Codex |
|---|---|---|
| 插件目录(skill、命令) | `--plugin-dir <目录>`;`[agent] auto` 的设置开启时,目录换成 `~/.local/share/aca/claude-plugins/<名字>/local/` 里的副本 | aca 在 `~/.local/share/aca/codex-plugins/<名字>/local/` 放一份 Codex 能识别的副本(斜杠命令转成 skill,Codex 没有自定义斜杠命令),每个账号的 `CODEX_HOME/plugins/cache/aca` 链接到那里(只创建一次,不修改 config.toml),启动时带 `-c plugins.<名字>@aca.enabled=true` |
| 插件的 MCP 服务 | 写进一份 `--mcp-config`(在 `~/.local/state/aca/hooks/mcp-*.json`) | 一份只含 `.mcp.json` 的副本 `~/.local/share/aca/codex-plugins/mcp-aca-<名字>/local/`,启动时带 `-c plugins.mcp-aca-<名字>@aca.enabled=true`;会话 id 通过 `env_vars` 从 Codex 自己的环境传过去(Codex 给 MCP 子进程的环境只包含白名单里的变量) |
| 提供给 agent 的能力 | MCP 服务 `aca`(`aca plugin tools`),工具名 `mcp__aca__<插件>__<能力>` | 同一个服务、同样的工具名,副本叫 `mcp-aca` |

- `<名字>` 是插件 id 里的点换成 `-`(`aca.delegate` → `aca-delegate`)。
- 两种 agent 的插件 MCP 都通过 `aca plugin mcp <插件 id>` 启动:aca 切换到插件目录、带上插件的环境(设置、`ACA_SESSION_ID`)后再执行 `[agent.mcp] command`。
- Codex 这条路依赖的是它内部的插件缓存布局,不是公开接口。`CODEX_HOME/plugins/cache/aca` 处已经有别的东西时 aca 不动它
  (只有指向的位置已不存在的链接会被替换),这次只挂载 MCP:用 `-c mcp_servers.<名字>={…}`,会话 id 写在 `env` 里,
  原因记在 `~/.local/state/aca/plugin-logs/mount.log`。
  Codex 的 skill 没有「只在输入时才进入上下文」的机制:skill 的名字和描述都会出现在它的 skill 列表里。
- 给 Codex 的 MCP 做成插件副本、而不用 `-c mcp_servers.<名字>={…}` 的原因:Codex(0.162、0.163)给对话生成标题时会另开一个线程,
  它关闭所有 MCP 的办法是在 `-c` 的同一层把整个 `mcp_servers` 替换掉。`-c` 给的服务在那里丢了命令,这个线程无法启动
  (`invalid transport in mcp_servers.<名字>`),对话就一直没有标题。插件带的 MCP 不在 `mcp_servers` 里,这个线程不会动它。
- 修改插件、开启或关闭插件、修改 plugins.toml 里的 agents 后:正在运行的会话要重启才会生效(Codex 只在启动时读取插件和工具列表)。

### 把能力作为工具提供给 agent

`expose` 里有 `agent` 的能力(以及你明确允许的 `agent?`,见「由你决定」),由 aca 自带的 MCP 服务 `aca` 包装成工具:参数说明按 `args` 生成(`string` / `path` → 字符串,
`int` → 整数,`bool` → 布尔,不带 `?` 的为必填),说明用 `desc`(没写则用 `title`)。agent 调用时,aca 按类型检查参数、
通过 `ACA_PLUGIN_ARGS_JSON` 交给命令,`ACA_SESSION_ID` 是发起调用的会话,标准输出和标准错误原样返回;退出码不为 0 的标记为出错。
单次最多运行 280 秒(Codex 默认最多等 300 秒):需要更久的,让命令先返回「仍在运行」,agent 稍后再查询。
只列出能作用于这个会话的插件(plugins.toml 的 `agents`)提供的工具。

### 事件钩子的 `if`

键是事件里的字段(`kind`,嵌套字段写成 `data.x`),以及这个会话的几项信息:`parent`(派出它的会话)、`tool`、`account`、`group`。
值相等才运行;写数组 = 匹配其中之一即可;`true` / `false` = 是否存在(非空)。例如只处理派出去的 worker 的临时错误:
`if = { kind = "transient", parent = true }`。条件不符时不会启动进程。

### 热键

`[[keys]]` 是界面里 `:` 菜单的键;`[[hotkeys]]` 是你在会话里面(面对 agent)时按的键。aca 把它绑定在 tmux 上,
和进入会话后用 Ctrl+Q 返回是同一种机制:

- **只在 aca 的会话里拦截**。在其他 tmux 会话里,这个键照常发给里面的程序。
- **不用前缀,往上翻看时也能按**:同时绑定在普通键表和 copy-mode 的两张键表里。按下后打开对应的窗格,作用于按键所在的会话
  (`ACA_SESSION_ID`),弹出在按键的那个终端上;窗格写了 `input` 的,照样能拿到会话的内容。
- **安装即生效,移除即失效**:绑定不写进 tmux.conf。插件安装或开启时绑定,关闭或删除时解除;tmux 重启后,下次新建会话、
  进入会话时补上。
- **不抢你的键**:这个键你已经在 tmux 里绑定了别的用途时,aca 不动它,并在 `aca plugin hotkey` 里指出;tmux 自带的翻看用的键也不会被覆盖。

键写 tmux 的键名,要带 Ctrl / Alt:`C-]`、`M-y`、`C-M-g`,或者 `F1`–`F12`。单个字符不行(会截走你输入的字);
`C-q`(aca 自己用的)、`C-c` `C-d` `C-z` 这些在终端里另有含义的不行,它们加上 Alt 的组合(`C-M-c`)也不行:在绑定还没补上时,
这个键发给 agent 的就是 Esc 加 Ctrl+C。一个窗格对应一个热键。
选键时注意:Ctrl 加字母几乎都被 agent 的输入框占用了(Claude Code 连 `C-]` 都用了),Alt 加字母常被窗口管理器截走;
在 copy-mode 键表里已有用途的键(vi 键位下的 `M-y`),往上翻看时按下仍是原来的用途。

## 由你决定

插件在清单里声明它想要什么,由你在 `plugins.toml` 里决定(也可以在命令行或插件页修改,都会写进这个文件):

```toml
[plugins."aca.tailnet"]
tools = ["list", "add"]            # 哪些能力作为工具提供给 agent。不写 = 清单里标了 agent 的那些;标了 agent? 的要在这里明确列出
keys = { mounts = "M", run = "" }  # : 菜单里的键:能力 / 窗格 / 视图 → 一个字符,"" = 不分配键;没写的按清单的 [[keys]]
hotkeys = { mounts = "M-m" }       # 会话里面的热键:窗格 → tmux 的键名,"" = 不要;没写的按清单的 [[hotkeys]]
```

- **给 agent 的工具**:aca 列出工具、实际调用时都按 `tools` 检查,不在其中的 agent 看不到也调用不了。清单里标 `agent?` 的
  (开放端口、删除文件这类会改动机器的)默认不提供。修改后正在运行的会话要重启才生效(Codex 只在启动时读取工具;gateway 开启时
  Claude 立即更新)。这是给 agent 的工具清单,**不是沙箱**:agent 有 shell 的话照样能直接运行 `aca` 命令。
- **键**:两个插件要同一个键时,你写的优先;都来自清单时,先登记的优先。没分配到键的不会悄悄消失 ——
  `aca plugin ls`、`aca plugin key`、插件页里会指出来,并告诉你在哪里修改。`/` 是 `:` 菜单自己的(筛选),不能分配。
- **热键**:分配规则相同(你写的优先)。清单没给热键的窗格,你也可以给它一个 —— 任何插件的任何窗格都可以。

```sh
aca plugin allow aca.tailnet             # 列出可提供给 agent 的工具:✓ 已提供 ○ 未提供(需明确允许的会标出)
aca plugin allow aca.tailnet add         # 提供;deny 撤回;--reset 恢复清单的设置
aca plugin key                           # : 菜单里的全部键,未分配到键的会标出
aca plugin key aca.tailnet mounts M      # 改成 M;--none 不分配键;--reset 恢复清单的设置
aca plugin hotkey                        # 会话里面的全部热键,未绑定的会标出
aca plugin hotkey aca.copy pick M-y      # 改成 Alt+y(tmux 上立即生效);--none 不要;--reset 恢复清单的设置
```

界面中:插件页(`P`)右边是高亮插件的详情,按 ⏎ 进入修改 —— 设置(⏎ 修改,开关类用 ⏎ 或空格切换,`-` 恢复默认)、
给 agent 的工具(空格切换)、插件菜单(在会话列表按 `:` 弹出的那个)里的键(按 ⏎ 后再按一个键改成它,⌫ 不分配键,
`-` 恢复清单的设置),Esc 返回列表。

## 你自己的插件

不想建插件、不想 link:写一份 `~/.config/aca/user/aca-plugin.toml`,放上就生效,删掉就失效(像在 kitty.conf 里写一行 `map`)。
写法和其他插件一样,id 固定为 `user`,name、version 可以不写;脚本放在旁边(`./x.sh` 按这个目录查找)。修改后立即生效,不需要 sync。

```toml
# ~/.config/aca/user/aca-plugin.toml
[[panes]]
id = "lazygit"
title = "lazygit"
cwd = "session"                  # 在会话的目录里打开
width = "95%"
height = "95%"
command = ["lazygit"]

[[commands]]
id = "links"
title = "回复里的链接"
input = "reply"                  # 最后一条回复从标准输入传入
command = ["sh", "-c", "grep -oE 'https?://[^ )]+' | sort -u"]

[[keys]]
key = "g"
run = "lazygit"
```

plugins.toml 里不用写它(sync 不会安装它,也不会删除它);要关闭它、修改它给 agent 的工具或键,同样写 `[plugins.user]`。
`aca plugin remove user` 删除不了它:要删除就删掉那个文件。

## 界面中

- **`:` 插件菜单**:在列表上按 `:`,列出已开启插件提供给 `menu` 的能力、窗格、视图,并按高亮行筛选 `contexts` 相符的
  (会话上是 `session`,分组标题上是 `group`,其他位置是 `global`;不写 `contexts` = 任何位置都可用)。↑↓ 选择、⏎ 运行,
  `[[keys]]` 分配了键的直接按那个键,`/` 筛选。作用于高亮的会话:`ACA_SESSION_ID` 就是它。
- **参数表单**:能力有 `args` 时先弹出一张表单,一个参数一栏(`bool` 用空格切换,带 `?` 的可以不填),按类型检查通过后才运行。
- **输出**:能力运行完,只有一行的输出显示在底栏,较长的放进全屏阅读页;视图总是放进阅读页,`r` 重新运行、`c` 复制、`Esc` 返回。
  都在后台运行(最多 120 秒),不影响其他操作。
- **窗格**:在 tmux 弹窗 / 分屏 / 新窗口中打开;界面不在 tmux 里时(比如手机直接 ssh 进来)就让出终端,在前台运行。
- **列表视图**(`rows = true`):打开为一页,每条一行,下方是高亮行的详情,最底下列出这一行可以按的键。模板里用到的字段
  这一行没有(或为空)时,这个键在这一行上不列出,按下会提示「此行不支持」。需要按两次的会先提示。键运行完,输出显示在列表下方
  (比如撤掉之后怎样加回去),列表重新读取;`r` 重新读取(插件没有占用 `r` 时)。
- **会话行上的文字**:`aca meta set` 设置的文字在列表里跟在标题后面,显示为灰色的 `[名字 值]`;名字以 `_` 开头的只显示值(适合图标、数字这类一看就懂的)。

### 列表视图的输出

```json
{
  "head": "mac.tail.ts.net\n发布 已关闭",        // 列表上方的灰色文字(可以多行)
  "warn": "tailscale 未登录",                     // 需要注意的内容(警告色)
  "empty": "暂无挂载",                            // 一行都没有时显示的文字
  "fields": { "publish_to": "on" },               // row = false 的键从这里读取
  "rows": [
    { "id": "https:443/", "mark": "●", "tone": "ok", "text": ":443 / → localhost:3000", "right": "监听中",
      "detail": ["https://mac.tail.ts.net/"], "fields": { "url": "https://mac.tail.ts.net/" } }
  ]
}
```

都可以不写。`tone` 是记号的颜色:`ok` / `warn` / `bad` / `dim`。模板里的 `{id}`、`{text}` 取自行本身,其他字段先在行的
`fields` 里找,找不到再看列表的 `fields`。`aca plugin view <插件.视图>` 把列表输出为每条一行(加 `--json` 原样输出)。

## 交给命令的环境

| 变量 | |
|---|---|
| `ACA_BIN` | 当前这个 aca 的完整路径。调用 aca 都用它 |
| `ACA_ENV` | `1` |
| `ACA_PLUGIN_ID` / `ACA_PLUGIN_ROOT` | 插件 id、插件目录 |
| `ACA_PLUGIN_CONFIG_DIR` | 存放用户可修改的配置(`~/.config/aca/plugins/<id>/`) |
| `ACA_PLUGIN_STATE_DIR` | 存放插件自己的状态(`~/.local/state/aca/plugins/<id>/`) |
| `ACA_PLUGIN_CONTEXT_JSON` | `{"session_id", "session_dir", "session_pane", "session_parent", "invocation_source"}` |
| `ACA_PLUGIN_SETTINGS_JSON` | 全部设置(清单的默认值,被 plugins.toml 里修改过的覆盖) |
| `ACA_SETTING_<名字>` | 每个设置一个变量(名字转大写,`-` `:` 换成 `_`)。字符串原样,其他类型按 TOML 的写法:`true`、`5`、`["a", "b"]` |
| `ACA_SESSION_ID` / `ACA_SESSION_DIR` | 作用于哪个会话、它的目录(没有会话时设为空值,而不是不设置) |
| `ACA_SESSION_PANE` | 那个会话里 agent 所在的窗格(`%5`):界面、命令行作用于一个未停止的会话时有,钩子在那个会话自己的 hook 里运行时有;agent 调用的工具里为空 |
| `ACA_SESSION_PARENT` | 派出它的会话(在某个会话里 `aca new` 出来的都挂在它下面);自己新建的为空 |
| `ACA_PLUGIN_ENTRYPOINT_ID` | 这次运行的是哪一项:能力、视图、窗格的 id(能力另外还有一个旧名字 `ACA_PLUGIN_ACTION_ID`) |
| `ACA_PLUGIN_ARGS_JSON` | 能力:传入的参数(按 `args` 的类型;没有参数时为 `{}`) |
| `ACA_ARG_<名字>` | 传入的参数再逐个放一份(名字转大写,`-` `:` 换成 `_`;没给的可选参数不设置):用 shell 写的插件不用解析 JSON。字符串原样,整数、布尔是 `3`、`true` |
| `ACA_PLUGIN_EVENT` / `ACA_PLUGIN_EVENT_JSON` | 事件钩子:事件名、整行事件 |
| `ACA_PLUGIN_TIMER` | 定时运行的能力:是哪个定时任务(`插件.能力`);其他情况为空 |
| `ACA_INPUT_FILE` | 写了 `input` 的窗格:会话的内容在这个文件里(写了 `input` 的命令、视图从标准输入读取)。`reply` 是 hook 记下的最后一条回复;没有记录(接管后还没重启的会话)或太长被截断时,aca 从它的对话记录里读取完整内容 |
| `ACA_INVOKED_BY` | 调用方:`menu` / `cli` / `agent` / `event` / `timer` |
| `ACA_LANG` | 界面当前的语言:`zh` / `en`。插件输出的文字按它选择语言 |

两个目录 aca 只负责创建,里面的文件、格式、清理都由插件自己管理(会话删除后需要清理的,订阅 `session.removed`)。
命令还会继承 aca 自己的环境(`PATH` 等),表里的变量覆盖在上面。

## 事件

一行一个 JSON,公共字段是 `ts`(毫秒)、`event`、`session`。

| 事件 | 自己的字段 | 什么时候发出 |
|---|---|---|
| `session.created` | `title` `tool` `account` `dir` `group` `parent` | 新建(分叉出来的多 `fork` 和来源会话 `fork_of`,`parent` 为空:没有派出它的会话;`aca adopt` 接管的多 `adopted` 和当前的会话名 `tmux`) |
| `session.started` | `how`:fresh / resume / fork | agent 已启动 |
| `session.stopped` | `idle`(因空闲被 aca 停止时:停止前空闲了几秒,见 config.toml 的 `[idle] stop_after`) | `aca stop`、界面里按 D;因空闲被 aca 停止 |
| `session.renamed` | `title` `tmux` | 用户重命名 |
| `session.titled` | `title` `tmux` | agent 自己设置或更换了标题 |
| `session.moved` | `group` | 移到别的分组 |
| `session.rehomed` | `tool` `account` `from`(原来的「工具:账号」) | 切换账号继续(`aca restart --as`、界面的 `X`):对话复制到新账号,在原地重启 |
| `session.archived` / `session.removed` | 删除的带 `title`;`aca release` 移出 aca 的多一个 `released`(tmux 会话保留) | |
| `turn.started` | `prompt` | 你发送了一句话 |
| `turn.done` | `last`(最后一条回复)`bg`(仍在运行的后台任务数)`secs` | 一轮结束 |
| `turn.failed` | `error` `kind`(transient = 限流、过载、连接中断,过一会儿继续多半就好;account = 额度、登录、欠费;fatal = 重试也无效) | 一轮失败(Codex 不报这个 hook:aca 查看状态时从它的对话记录中读出来补上) |
| `turn.interrupted` | `by`(`aca interrupt` 中止 Claude 时:谁按的) | 你按 Esc 中断了它(Codex 自己报);或者 `aca interrupt` 中止了它 |
| `agent.waiting` | `ask` `kind`(choice / permission / 空 = 无法识别)`options`(选择题的选项) | 等待你确认或回答 |
| `agent.exited` | `reason` | agent 进程退出 |
| `conversation.changed` | `conv` `from` | 对话 id 变了(/clear) |
| `message.sent` | `id`(这条消息的)`from` `kind`(msg / notice)`what`(通知所说的事;不是通知时为 null)`wake` `text` `at`(定时的到点时刻,秒;不是定时的为 null)| 往这个会话的收件箱放了一条消息(`aca msg`,或者它派出去的 worker 自动发的通知)。定时的(`aca msg --in`)放进去时就发这一条,送达的那条要等到点 |
| `message.delivered` | `id` `how`(bell / queue / type / wait) | 那条消息已送达 |
| `message.throttled` | `from` `count` `secs` | 同一个会话发送过于频繁,之后的只保存、不唤醒 |
| `ask.answered` | `by`(谁代为回答;`you` = 在外部回答)`answer` `ask` `kind`(choice / permission;启动时的确认框是 `gate`) | `aca answer` 代为回答了确认框 |
| `limits.changed` | `tool`(claude / codex)`name`(登记的账号名,未登记的是目录名去掉点)`dir`(账号目录)`wins`(这个账号的每个窗口:`label` 5h / 7d / Fable …、已用百分比 `used`、`resets` 和这份数据的时间 `at`,都以秒计) | 一个账号的订阅额度有变化:刷新时数值没变的不发出。Claude 的来自状态栏,Codex 的在它一轮结束时读取。`session` 是报告它的会话,不是 aca 启动的则为空;一次获取全部账号用 `aca limits --json` |

全部事件记录在 `~/.local/state/aca/events.jsonl`;`aca events -f` 持续读取。

## 定时

`[[timers]]` 每隔 `every` 运行这个插件的一个能力。aca 没有常驻进程,到点运行靠 macOS 的 launchd:每个已开启的定时任务对应一个
LaunchAgent(`~/Library/LaunchAgents/com.aca.timer.<插件>.<能力>.plist`),到点运行 `aca plugin timer tick <插件.能力>`。
合盖睡眠期间错过的几次,唤醒后只补运行一次。

- **开关**:没写 `manual` 的随插件开启(链接、安装、更新、开启或关闭、删除插件时,aca 按清单核对一遍)。`manual = true` 的平时关闭,
  由插件自己开关:有任务时运行 `"$ACA_BIN" plugin timer on "$ACA_PLUGIN_ID.collect"`,完成后 `off` —— 没有任务的日子里什么都不运行。
  插件关闭时都不运行,重新开启后恢复原来的开关状态。
- **运行时**:环境和从命令行运行这个能力相同,另有 `ACA_INVOKED_BY=timer`、`ACA_PLUGIN_TIMER=<插件.能力>`,没有会话(`ACA_SESSION_ID` 为空),
  标准输入为空。输出写进插件的日志(`aca plugin log`),最多运行 10 分钟(超时先向它的进程组发 TERM,3 秒内不退出再发 KILL);上一次还没结束时,下一次不启动。
- **在运行中开关它自己**(比如完成后 `off`):aca 等这一次运行结束再改动 launchd,不会中途把自己卸掉。
- PATH 在开启的那一刻复制进 plist(launchd 自己的 PATH 里没有 Homebrew 的路径)。
- 测试、基准测试(设置了 `ACA_HOME` 或 `ACA_TMUX_SOCKET`)不改动 launchd:plist 写在 aca 状态目录的 `timers/` 里,视为已安装。

```sh
aca plugin timer                       # 每个定时任务:● 已开启 ○ 已关闭、间隔、上次运行的结果;--json 每行一个
aca plugin timer on | off <插件.能力>   # 开启 / 关闭(没写 manual 的随插件开关,不能单独关闭)
aca plugin timer run <插件.能力>        # 立即运行一次,和到点时一样:直接输出,关闭状态下也运行
```

`--json` 每行一个:`{"id", "every"(秒), "manual", "on"(是否应开启), "enabled"(插件是否开启), "scheduled"(launchd 那边是否已安装),
"last"(上次开始的时间,0 = 从未运行), "secs", "rc"(退出码;超时被停止 / 无法启动时为 null), "why"}`。

## 出错时

钩子和定时任务在后台运行,没人看着:上次没有成功(退出码不为 0、无法启动、超时),或者 gateway 下的服务连续失败 5 次后停止了,
aca 会指出是哪个插件:

- tmux 状态栏那一段(`aca tmux-status`)后面多出 `⚠ bark`(aca 自带的插件省略 `aca.`;最多显示两个名字,其余写成 +N);
- 界面顶栏右边一格 `⚠ 1 插件出错(P)`,插件页那一行标「出错」,右边写出是哪一处、什么问题、什么时间;
- `aca plugin ls` 每处一行:`⚠ me.hello:钩子 turn.done 退出码 3 · 5m 前(aca plugin log me.hello)`。

钩子是不等待的:aca 启动它时外面包一层 sh,运行结束后把退出码记到 `~/.local/state/aca/plugin-health/<插件>/hook-<序号>`。
只统计已开启的插件、一天以内的记录;同一处下一次运行成功就不再提示,插件关闭或删除后不提示,重新链接或更新时(清单可能变了,序号对不上)清空记录。
所以退出码就是插件表示「这次没有完成」的方式:没完成就不要 `exit 0`;只是没有任务可做,就 `exit 0`。

## 插件常用的命令

```sh
"$ACA_BIN" show "$ACA_SESSION_ID" --json     # 标题、状态、目录、对话 id、最后一条回复、是否有人连着
"$ACA_BIN" ls --json --no-screen             # 全部会话
"$ACA_BIN" send <id> -m '…' --wait 600       # 等它空闲后再往输入框发送一段话
"$ACA_BIN" meta set <id> <名字> <值> [--ttl 秒]   # 在会话行上显示一小段文字
"$ACA_BIN" meta rm <id> <名字>
"$ACA_BIN" msg <id> < 话.txt                  # 放进它的收件箱,在它方便时送达(运行中的等这一轮结束)
"$ACA_BIN" plugin timer on | off "$ACA_PLUGIN_ID.<能力>"   # 开关自己的定时任务(manual 的)
"$ACA_BIN" new / start / stop / fork / rename / move / rm
```

`meta set` 是插件往列表里放内容的方式:名字为 1–32 个字母、数字、`_`、`-`,值截断到 80 个字,`--ttl` 到期后自动消失。
在 `aca ls` 里显示为 `[名字 值]`(名字以 `_` 开头的只显示值),在 `--json` 里位于 `tokens`。

## 接口约定(api 1)

清单里的 `api = 1` 表示插件按这一版编写(不写也算 1;比当前 aca 新的无法读取,链接时会提示先升级 aca)。同一版内 aca 只增加、不修改:

- **会增加**:新的可选字段、新的环境变量、新的事件、事件和 JSON 里的新字段。读取 JSON 时跳过无法识别的字段,不要因为多了一个字段就报错。
- **不会变**:下面这些的名字、含义、类型。要删除或改名就是 `api = 2`。

| 稳定的部分 | 见 |
|---|---|
| 清单的字段 | 「清单」 |
| 交给命令的环境变量 | 「交给命令的环境」 |
| 定时、`aca plugin timer --json` | 「定时」 |
| 事件和它们的字段 | 「事件」 |
| 列表视图的 JSON | 「列表视图的输出」 |
| 给 agent 的工具:名字、参数、返回值 | 「把能力作为工具提供给 agent」和下文 |
| `aca ls --json` / `aca show --json` 的外层、`aca plugin actions --json` | 下文 |
| 输出、退出码、超时 | 下文 |

**输出、退出码、超时**

- 退出码 0 = 成功,其他都算失败,没有别的含义。
- 标准输出是结果。标准错误:命令行、agent 的工具、界面里的能力和视图,接在标准输出后面一起显示;**列表视图运行成功时只读取标准输出**
  (那是 JSON),标准错误记进插件的日志(`aca plugin log`);运行失败时两者都显示。
- 界面里运行的、`aca plugin view` 最多 120 秒,agent 的工具 280 秒,定时任务 10 分钟:超时先向它的进程组发 TERM,3 秒后仍不退出就 KILL。
  `aca plugin run`、事件钩子、窗格不限时。
- 命令自己退出后,留在后台的进程仍持有它的输出:aca 收下已读到的内容就返回,不会继续等待。需要在后台常驻的,把输出重定向到别处。
- 标准输入:从命令行运行时接你的终端;界面、agent 的工具、钩子、定时任务中为空;写了 `input` 的从这里读取会话的内容。

**aca 输出的 JSON**

- `aca ls --json`(数组)、`aca show <id> --json`(单个)的稳定字段:`id` `title` `state`(running 运行中 / ask 等待回应 /
  unread 未读 / idle 空闲 / error 出错 / exited agent 已退出 / stopped 已停止)`tool` `account` `group` `dir` `parent`(派出它的会话)
  `fork_of`(分叉出来的会话的来源;分叉出来的没有 `parent`)
  `tmux`(tmux 会话名)`pane`(agent 所在的窗格)`attached`(连着的终端数)`conv`(对话 id)`ask`(等待回应时的提问)
  `ran`(这一轮已运行的秒数)`active`(最近有动静的时间)`bg`(后台任务数)`locked` `created` `order` `tokens`(`meta set` 设置的文字)
  `idle_stopped`(因空闲被 aca 停止的时间,列表里显示 ☾;其他情况为 0)。`aca show --json` 另有 `work`:它真正在干活的目录
  (agent cd 进去的目录,或最近改的文件所在的仓库;都查不到时同 `dir`)。
- `aca show --json` 里的 `run` 是 aca 内部记录的状态,**不是接口**,其中的字段随时可能变。
- `aca plugin actions --json`:每行一个能力,`{"id", "title", "contexts", "args", "expose"}`。
- 时间:事件的 `ts` 是毫秒,其他地方是秒。JSON 是紧凑的一行,用 `jq` 读取,不要用 sed 拆分(排版格式不属于接口)。

**命名**

- 给 agent 的工具名是 `<插件 id,点换成 ->__<能力 id>`。所以给 agent 用的能力 id 里不能有 `:` 和 `__`;
  两个插件的 id 转换后相同(`a.b` 和 `a-b`)时不能同时链接。
- 参数名、设置名转成环境变量时转大写、`-` `:` 换成 `_`:转换后相同的两个名字不能同时使用。
- 能力、窗格、视图共用一套 id(`[[keys]]`、命令行都只按 id 查找)。
- 旧写法仍然可用,文档只写新写法:`[[actions]]` = `[[commands]]`,`ACA_PLUGIN_ACTION_ID` = 能力的 `ACA_PLUGIN_ENTRYPOINT_ID`,
  `[agents.claude] plugin_dir` = `[agent] dir`。

## 第一方插件

在仓库的 [`plugins/`](../plugins) 里,编译进 aca 一起发布,默认都不安装。`aca plugin install aca.<名字>` 不用写路径;
plugins.toml 里只写一个 `[plugins."aca.core"]` 表头(不写 source),换台机器 `sync` 也能装上。
安装 = aca 把编译进来的文件写到 `~/.local/share/aca/first-party/<名字>/` 再登记;这份文件由 aca 管理,升级 aca 后内容有变化时,
下次启动 agent、`sync`、打开插件页时按新版本重写(插件页里标「自带」,`aca plugin ls` 里标「aca 自带」)。

| 插件 | 提供什么 |
|---|---|
| `aca.delegate` | `/aca:delegate`(Claude 中是斜杠命令,输入 `/dele` 补全出来的就是它;Codex 中是一个 skill):主会话规划、经 aca 派出 worker、验收、汇报;worker 遇到临时错误时自动继续。还带一个 `/aca:spec`:一个变更从提案到归档(`openspec/changes/<slug>/`),每条任务交给 `/aca:delegate` 完成,在真实环境中验收通过才算完成。设置:`worker`(派给谁,默认 `codex`)、`language`(用哪种语言回复你,空 = 跟随你)、`say`(代为发送的那句话)、`max_continue`、`delay` |
| `aca.core` | 把 aca 的核心能力作为工具提供给 agent(Claude、Codex 都支持):`sessions` 查看会话、`dispatch` 派出 worker(挂在调用者下面,可以带 worktree 分支)、`message` 发送消息、`wait` 等待(每次最多 240 秒)、`answer` 代为回答、`interrupt` 中止、`wakeup` 过会儿叫醒自己(定时消息发给自己,`aca msg --in`)。代为回答、中止的对象只能是调用者自己派出的会话。Codex 另外带一个 `wakeup` skill:要等好几分钟的活不在这一轮里干等,结束这一轮,让 aca 过会儿叫醒它回来看 |
| `aca.gateway` | MCP gateway,见下一节 |
| `aca.remote` | 远端任务,全部实现在插件脚本里(sh + jq + curl + ssh):把长任务派到别的机器上后台运行,结束时结果作为一条消息放进派出它的会话的收件箱。`:` 菜单里 `r` 派出、`t` 列表(⏎ 复制查看日志的命令、`x` 撤销、`r` 立即获取)、「测试 server」;agent 有 `run`、`list` 两个工具。回调先发到一台始终在线的 server 上([jobhook](https://github.com/cheny-00/jobhook)),设置:`url`、`token`。有等待中的任务时才开启每 5 分钟拉取一次的定时任务 |
| `aca.tailnet` | tailnet 挂载,全部实现在插件脚本里(sh + jq,需要 `jq`):`:` 菜单里 `m` 是列表,每条 `tailscale serve` 一行(显示目标端口是否在监听),⏎ 复制地址、`x` 撤销(并写出如何加回去)、`a` 把本机端口挂载出去、`p` 开启 / 关闭发布(需安装 tpub)。agent 可以列出;挂端口是 `agent?`,需你明确允许(`aca plugin allow aca.tailnet add`) |
| `aca.bark` | 一轮结束、等待你确认、出错时通过 Bark 推送到手机(有人正连着那个会话时不推送)。设置:`url`(推送地址)、`workers`(派出去的 worker 也推送)、`group`;发送一条测试推送:`aca plugin run aca.bark.test` |
| `aca.discord` | 同样这三种情况发到 Discord 频道(webhook;有人正连着那个会话时不发)。设置:`url`(webhook 地址)、`mention`(等待确认时 @ 的用户 id)、`workers`、`username`。只往外发;在 Discord 里回复并发送到会话尚未实现 |
| `aca.notify` | 同样这三种情况在本机弹出桌面通知(你正连着那个会话时不弹)。设置:`command`(换一个弹通知的命令,标题、正文是它的两个参数;为空或它发送失败时,macOS 用 `osascript`、Linux 用 `notify-send`)、`workers`、`attached`(正连着时也弹);显示一条测试通知:`aca plugin run aca.notify.test` |
| `aca.copy` | 复制回复里的命令,全部实现在插件脚本里(sh,需要 `fzf`):在会话里按 Ctrl+Alt+Y(热键;`:` 菜单里是 `y`),从最后一条回复的原文中识别代码块和行内命令 —— 只有一条时直接复制,多条时弹出列表按数字键选择;shell 代码块里的每一条也单独列出。复制到 tmux 缓冲区、外层终端(OSC 52)和本机剪贴板。设置:`inline`(行内的 `…` 要哪些)、`lines`、`copy`(换一个复制命令) |

## MCP gateway

第一方插件 [`plugins/gateway`](../plugins/gateway)(`aca.gateway`)是 MCP 的注册中心。安装并开启后:

- agent 上只挂一个 MCP 服务 `aca`:一个很薄的转接层(`aca plugin gateway connect`),从 stdio 进,经本机 Unix socket 连到 gateway。
- gateway 下面挂着:插件的 MCP(`[agent.mcp]` 中 `via` 不是 `direct` 的)、插件提供给 agent 的能力、config.toml 里
  在 gateway 的设置 `servers` 中列出的 `[mcps.<名字>]`。工具名是 `<服务>__<工具>`(能力的工具名本来就是 `<插件>__<能力>`,不再加前缀)。
- 每个服务在第一次被用到时才启动。`scope` 为 `shared`(默认)的只启动一份,所有会话共用;为 `session` 的每个会话一份,
  该会话断开就回收(有状态的服务用这个,比如浏览器)。连接失败时退避重启,连续失败 5 次后停止。
- 识别调用者:转接层连上时先报告所属会话;gateway 转发调用时在 `_meta` 里加上 `dev.aca/session`。共用的插件 MCP 从这里读取调用者
  (每个会话一份的服务,环境里还有 `ACA_SESSION_ID`)。每个会话能看到的工具按 plugins.toml 的 `agents` 计算。
- gateway 是按需启动的进程:第一个转接层发现它没在运行时就启动它(加锁,只启动一个),没有连接超过 `idle_minutes` 分钟后自动退出。
  状态在 `~/.local/state/aca/gateway/`:每个服务一份日志(`logs/`),每次调用一行(`calls.jsonl`)。

```toml
[plugins."aca.gateway"]
source = "~/src/aca/plugins/gateway"
settings = { servers = ["web", "playwright"], idle_minutes = 10 }
```

```sh
aca plugin gateway status            # 是否在运行、谁连着、每个服务的状态、最近的调用
aca plugin gateway restart [服务]     # 重启下面的一个服务(下次被用到时再启动)
aca plugin gateway log <服务>         # 它的 stderr
aca plugin gateway stop
aca plugin gateway add | drop <名字>  # 把 config.toml 的 [mcps.<名字>] 加入 / 移出 gateway
aca plugin gateway off | on <服务> [--session id]   # 只对一个会话关闭 / 开启一个服务
aca plugin gateway import [--apply]  # 列出各账号里单独配置的 MCP;--apply 加入 gateway
```

- **界面**:在插件页(`P`)的 gateway 那一行按 `m` 进入,每个服务一行(运行中 / 空闲 / 已崩溃、工具数、每天调用次数、使用它的会话数),
  空格把 `[mcps.*]` 加入 / 移出,`r` 重启、`l` 查看日志、`s` 停止。`M` 页最上面是这个会话从 gateway 能看到的服务,
  按空格只对这个会话关闭 / 开启(Claude 立即生效,Codex 重启后生效);已加入 gateway 的 `[mcps.*]` 不再在「项目」里重复列出。
- **把账号里的 MCP 加入 gateway**:`import` 读取 Claude 账号目录里 `.claude.json` 顶层的 `mcpServers` 和 Codex 账号目录里
  `config.toml` 的 `[mcp_servers]`,同名且写法相同的合并成一个;HTTP 的(gateway 目前只支持 stdio)、几处写法不同的会标出来,不加入。
  `--apply` 写进 aca 的 config.toml `[mcps.<名字>]`(已有的不修改)再加入 gateway。账号里的那份保持原样:
  gateway 开启时 agent 会同时看到两份,确认没问题后请自己去账号里删除。

关闭这个插件后:各个服务恢复为直接挂到 agent 上(正在运行的会话重启后生效)。

## plugins.toml:要用哪些插件

`~/.config/aca/plugins.toml` 列出要用哪些插件,就像 vim-plug 的列表写在 vimrc 里。它可以和 dotfiles 放在一起,换台机器运行一次 `aca plugin sync` 即可。

```toml
[plugins."aca.delegate"]
source = "~/src/aca/plugins/delegate"   # 本地目录 = 链接;git 地址 / owner/repo = 克隆
agents = ["claude:*", "codex:work"]    # 给哪几个「工具[:账号]」带入内容;不写 = 清单里列出的几种 agent 全给
settings = { max_continue = 3 }        # 覆盖清单里的默认值
tools = ["dispatch"]                   # 给 agent 哪些工具(见「由你决定」)
keys = { tasks = "T" }                 # : 菜单里的键

[plugins."owner/gh-glance"]
source = "owner/gh-glance"
ref = "v1.2"                           # 固定在一个标签 / 分支 / 提交上
enabled = false                        # 暂时关闭
```

- 在命令行和界面(`P`)里安装、删除、开启或关闭、修改设置时都会同步修改这个文件:只改那个插件的那几行,其他内容和注释保持原样。
- 你手动修改后:`aca plugin sync` 让本机与它一致 —— 缺的安装、没写的删除、开关按它设置。在界面的 `P` 页按 `e` 打开它
  (定位到高亮插件那一段),改完返回后自动同步;在别处修改的(比如从 dotfiles 拉下来的),插件页底部会列出与本机的差异,按 `S` 同步。
- `plugins.lock` 记录每个通过 git 安装的插件对应的提交:`sync` 按它安装(换机器装出来的版本一致),`update` 拉取新版本后更新它。
- 修改设置、`agents` 后不用 sync:下一次运行命令、启动 agent 时就会使用新值。
- 还没有这个文件时,第一次用到时按当前登记的插件生成一份。

## 安装和调试

```sh
aca plugin install aca.delegate      # aca 自带的插件,不用写路径;安装后列出它提供的内容
aca plugin install owner/repo        # GitHub 上的;也可以是任何 git 地址。克隆到 ~/.local/share/aca/plugins/<id>
aca plugin install <git 地址> --ref v1.2   # 固定在一个版本上(update 不会更新它)
aca plugin update [id]               # 拉取 git 安装的插件的新版本(只快进);不给 id = 全部。新的提交记进 plugins.lock
aca plugin sync                      # 让本机与 plugins.toml 一致
aca plugin remove <id>               # 不再使用:git 安装的连目录一起删除,链接的目录保留;解除它的热键。插件自己的配置、状态保留(空目录不保留)
aca plugin new ./my-plugin           # 创建一个新插件(清单 + 脚本);--id 指定 id,不指定则为 me.<目录名>
aca plugin check [目录 | id]          # 检查:清单(包括无法识别的字段)、要运行的程序和脚本、needs;有问题时退出码为 1
aca plugin link ./my-plugin          # 链接本地目录(正在编写的插件;修改清单后需重新 link)。install 一个本地目录也等于 link
aca plugin ls                        # ● 已开启  ○ 已关闭;来源;给哪些 agent 带入了内容;哪些正在运行的会话需要重启才能带上
aca plugin actions                   # 能力、窗格、视图
aca plugin run example.git-glance.refresh top=5 --session 3f2a   # 参数写成 名字=值
aca plugin view aca.remote.tasks     # 运行一个视图,输出结果
aca plugin setting aca.delegate [worker [codex:work]]   # 列出全部设置 / 读取一个 / 修改一个(写进 plugins.toml);--reset 恢复默认
aca plugin allow | deny <id> [能力…]    # 给 agent 哪些工具(见「由你决定」)
aca plugin key [<id> <目标> <键>]       # : 菜单里的键
aca plugin pane example.git-glance diff --session 3f2a [--placement window]
aca plugin log example.git-glance    # 事件钩子的输出;列表视图的标准错误也记在这里
aca plugin emit turn.failed kind=transient [--session id] [--plugin id]   # 模拟一个事件,钩子在前台运行
aca plugin timer run aca.remote.collect   # 立即运行一次定时任务,和到点时一样,直接输出
aca plugin disable / enable / unlink <id>
```

事件钩子在 hook 进程里启动,**不等待它运行结束**,输出写进 `plugin-logs/<id>.log`。动作在前台运行,输出直接交给调用者。
窗格:`popup` 在 tmux 弹窗里打开(不在 tmux 里时直接在当前终端前台运行),`split` 在 agent 所在窗格的右边打开,`window` 是那个 tmux 会话里的新窗口。

例子在 [`plugins/examples/`](../plugins/examples):`git-glance`(事件 + 动作 + 窗格 + 会话行上的文字)。第一方插件也是现成的例子,
`aca.tailnet`、`aca.remote` 最完整(只用插件接口,不依赖 aca 内部的代码;remote 还用了定时任务)。

aca 会把启动 agent 时插件提供的内容记在会话的运行时状态里(`mounts`);之后安装、删除、开启或关闭插件,或者修改 `agents`,
`aca plugin ls` 和插件页会列出哪些正在运行的会话启动时带入的内容和现在不同:重启它们才能带上。

[`plugins/delegate/`](../plugins/delegate)(`aca.delegate`)是一个可以直接用的插件:给 Claude 带一个 `/aca:delegate` 命令
(输入 `/delegate` 补全出来的就是它),给 Codex 一个叫 delegate 的 skill —— 主会话规划、用 `aca new -w` 把任务派给 worker
(设置 `worker`,默认 `codex`;每个 worker 在自己的 worktree 里)、用 `aca wait` 等待它(Claude 放在后台,Codex 在前台用 `--any`)、
自己验收、汇报;派出去的 worker 某一轮因为限流 / 过载失败时,它的事件钩子过一段时间代为发送一句话(设置 `say`,
最多 `max_continue` 次,间隔 `delay`)。同一个插件还带 `/aca:spec`(Codex 中是叫 spec 的 skill):一个变更的提案、设计、
任务记在 `openspec/changes/<slug>/`,每条任务交给 `/aca:delegate`,发布到环境中、确认已上线、在真实环境中验收通过才算完成,然后归档;
派活的 brief、worker 的结果、验收记录放在 `doc/spec/<slug>/`。仓库自己的检查、发布、真实环境中怎么验收,写在仓库的
`.claude/spec-profile.md`。安装:`aca plugin install aca.delegate`。

## 和 herdr 插件的区别

| herdr | aca |
|---|---|
| `herdr-plugin.toml` | `aca-plugin.toml`,动作 / 事件 / 窗格三部分的写法相同;另外有能力的参数、设置、事件的 `if`、给 agent 的部分 |
| `HERDR_*` 环境变量 | `ACA_*`,一一对应 |
| 常驻的 server,socket API,`events.subscribe` | 没有常驻进程:事件写进一个只追加的文件,用 `aca events -f` 读取 |
| `pane.report_metadata` 的令牌 | `aca meta set` |
| 窗格 placement:overlay / popup / split / tab / zoomed | popup / split / window(都是 tmux 自己的) |
| `[[startup]]` | 没有:没有 server 需要等待。需要定期运行的写 `[[timers]]`(aca 代为安装 launchd 任务) |
| `[[keybindings]]` | `[[keys]]`:绑定在界面的 `:` 菜单里(按 `:` 再按那个键),不和 aca 自己的键冲突;只是默认值,你可以在 plugins.toml 里修改 |
| | `[[views]]`:命令的输出显示在 aca 的全屏阅读页,插件不用自己写界面 |
| `plugin install <GitHub>` | `aca plugin install owner/repo`(任何 git 地址都可以,`--ref` 固定版本),`update`、`remove` |
| | `[agent]`:给 aca 启动的 agent 带一个插件目录(skill、斜杠命令)和一个 MCP 服务 |
| | `plugins.toml` + `plugins.lock` + `aca plugin sync`:声明式管理;给 agent 的工具、键也在这里由你决定 |
| | `input`:把会话的最后一条回复 / 屏幕内容交给插件(像 kitty 的 kitten 读取窗口内容那样) |
| | `~/.config/aca/user/`:不建插件也能添加小功能 |
| `[[link_handlers]]`、`[[build]]` | 尚未实现 |
