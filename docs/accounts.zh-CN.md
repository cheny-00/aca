# 账号

[English](accounts.md) | 简体中文

怎样用多个账号同时运行同一个工具,以及怎样看到 Claude 的订阅额度。

一个账号其实就是**「以这个身份启动工具时,你需要额外输入的那些东西」**:
换一个配置目录、设几个环境变量、加几个参数。你只需要写一次,之后 aca 每次启动时都会替你加上。

如果你只用默认的 `~/.claude` 和 `~/.codex`,什么都不用登记。

```toml
[accounts.claude]
personal = "~/.claude-personal"           # 只是换了一个配置目录
work = "~/.claude-work"
bedrock = { env = { CLAUDE_CODE_USE_BEDROCK = "1", AWS_PROFILE = "work" } }   # 目录不变,换一种认证方式

[accounts.codex]
work = "~/.codex-work"
team = { home = "~/.codex-team", args = ["--profile", "team"] }              # 配置目录,加上 Codex 自己的 profile
api = { env_file = "~/.secrets/openai.env" }                                  # key 存在文件里,每次启动时读取
```

| 配置项 | 含义 |
|---|---|
| `home` | 配置目录。通过工具自己识别的环境变量传给它(`CLAUDE_CONFIG_DIR` / `CODEX_HOME`) |
| `env` | 环境变量。值开头的 `~/` 会被展开成你的家目录 |
| `env_file` | 一个文件,每行一个 `KEY=值`。aca 不保存里面的内容,每次启动时现读 |
| `args` | 每次启动会话时加上的参数 |

最常见的情况(只是换一个配置目录)不需要手动编辑文件。下面几种方式写入的是同一行配置:

```sh
aca account add claude work ~/.claude-work   # 不写目录的话,默认是 ~/.claude-work
aca setup                                    # 在那个页面里按 +
                                             # 或者在界面里:Ctrl+S,再按 a,再按 +
```

之后用 `claude:work` 来指定它:

```sh
aca new claude:work ~/code/app
aca run claude:work                       # 新建的目录需要登录一次:这条命令会启动 Claude,让你完成登录
aca run codex:work -- login               # Codex 的登录
aca account status                        # 各个账号是否已登录(Claude 账号看它存下的登录在不在,其余向工具本身查询;aca 从不读取凭据的内容)
```

几点说明:

- **没有指定账号时:** `aca new codex` 不会添加任何东西,工具使用它自己默认的目录和登录。
  想固定一个默认账号,就设置 `[tools.codex] account = "…"`。
- **不登记也能用:** 在冒号后面直接写目录,例如 `aca new codex:~/.codex-try`。
- **账号之间互不影响。** 如果一个会话是从另一个 agent 的会话里启动的,
  aca 会把外层会话留在环境变量里的身份信息去掉(例如 `CLAUDE_SECURESTORAGE_CONFIG_DIR`,
  它会让新启动的 Claude 使用外层账号的登录;还有 `ANTHROPIC_PROFILE`)。
  你写在账号自己的 `env` 里的变量会保留。
- **工具自己的命令**(登录、管理 MCP 等)通过 `aca run` 运行:`aca run claude:work -- mcp list`。
  它会带上账号的 `home`、`env` 和 `env_file`,但不带 `args`,因为 `args` 只用于启动会话。
- **你自己的 `--settings` 文件会被保留。** Claude 只使用它收到的最后一个 `--settings`,
  所以如果你传了自己的,aca 会把它的 hook 合并进你那份文件的副本,再把副本传给 Claude。你的原文件不会被修改。
- **删除账号**(`aca account rm claude work`)只是去掉 `config.toml` 里的那一行。
  配置目录和其中的登录都不会改动。
- **重新登录会覆盖当前的登录。** 工具的登录命令一启动,就会丢弃现有的登录
  (`codex login` 会立刻删除 `auth.json`)。对已经登录的账号,账号页面会先向你确认。
- aca 从不保存或复制凭据,也从不往这些配置目录里写东西。唯一的例外需要你主动选择:下面说的状态栏设置。

## 放在沙箱里的账号

适用于必须和这台 Mac 隔开的账号,比如公司发的账号。它放在一个经 ssh 进入的笼子里(容器、虚拟机、另一台电脑),
aca 在里面启动它的会话。Mac 上的目录用同一个路径挂进笼子,两边的路径意思一样。

```toml
[sandbox.work]
ssh = "work"                     # ~/.ssh/config 里的 Host
up = "~/cage/up"                 # ssh 连不上时先运行它
dirs = ["~/work/app"]            # 挂进去的项目目录
workspace = "~/cages/work"       # 也挂进去:claude/ 是笼子里 Claude 的配置目录,worktrees/ 放它的 worktree
space = "3"                      # 它的会话放进界面的哪个工作区

[accounts.claude]
team = { sandbox = "work" }      # 不写 home:它的配置目录是笼子里的那个

[accounts.codex]
team = { sandbox = "work" }      # Codex 也一样
```

- `aca new claude:team ~/work/app` 经 ssh 在笼子里运行 Claude。这个会话和别的会话一样能列出、预览、进入。
  带进去的只有工具自己的参数、`[sandbox.work]` 和账号的 `env`。hook 和 Mac 上的其他文件不带进去,所以它的状态来自 Claude 自己的上报和屏幕。
- 它的会话只能在 `dirs` 列出的目录里启动,或者在 workspace 的 `worktrees/` 里;`-w` 新建的 worktree 就放在那里。
- `aca run claude:team` 也在笼子里运行,所以登录也在笼子里完成。登录和对话记录在 `<workspace>/claude/` 里,
  这台 Mac 读得到。
- Codex 也一样,配置目录是 `<workspace>/codex/`。`aca run codex:team login` 在笼子里登录,用这台 Mac 的浏览器:
  打开它打印的链接,登录完浏览器跳回的地址会转进笼子,不用再复制什么。通过 SSH 连到这台 Mac 时改用设备码
  (ChatGPT Business / Enterprise 工作区要管理员先允许设备码登录)。它在用哪个对话,要等重启时才知道:
  aca 取它在会话目录里最新开始的那个对话。目前只有 Claude Code 和 Codex 的账号能绑定沙箱。
- `aca new shell:work` 在笼子里开一个终端,不用另外配置;界面里按 `^E` 就有这一项。
  会话目录挂进去了就从那里开始,否则落在笼子登录时的默认目录。
- **挂进去的目录上的 git 在笼子里运行。** 笼子能往仓库的 `.git/config` 和 hooks 里写命令,Mac 上的 git 会以你的身份运行它们。
  所以 aca 自己对挂进去的目录运行 git 时(看改动、worktree、状态栏),都经 ssh 在笼子里运行。
  你自己在 Mac 上用这个目录(编辑器、自己的 git),仍然等于信任笼子写进去的东西。
- **封闭。** aca 不往里送任何东西:不从别的会话启动它的会话(派活),不从别的会话传话,不发 worker 的通知,
  对话不换到别的账号,也不从别的账号换进来,`aca.remote` 也不往里派任务(不管用哪个能连到它的名字)。你自己仍然可以给它的会话发话。这只约束 aca:
  你的 agent 如果读得到 ssh 钥匙,还是能自己连进去。
- **接活。** `[sandbox.work]` 写 `delegate = true`,别的会话就能往里派活、给它传话,`aca wait`、`--notify` 和平常一样。
  干完的结果会回到派活的会话:笼子里的东西会到这台 Mac 上的 agent 面前。笼子里没有 hook,一轮的开始和结束来自 agent 自己的上报。
  换账号、`aca.remote` 照旧封闭。

### 不进笼子:在本机换时区、换出口

不写 `ssh` 的 `[sandbox.*]` 在本机跑,只是给绑定它的账号带上它的 `env`。适用于只要求换时区、经公司代理出去的账号。

```toml
[sandbox.unis]
env = { TZ = "America/Los_Angeles", HTTPS_PROXY = "http://127.0.0.1:28888", HTTP_PROXY = "http://127.0.0.1:28888", NO_PROXY = "localhost,127.0.0.1" }
workspace = "~/cages/unis"       # 绑定它的账号的配置目录:codex/、claude/

[accounts.codex]
team-unis = { sandbox = "unis" }
```

- 它的会话和普通账号一样在本机启动,hook、插件、派活、传话、换账号都照常,只是多了这组变量,配置目录是
  `<workspace>/codex/`。账号自己的 `env` 优先于它的。`aca run codex:team-unis login` 在本机登录。
- `aca new shell:unis` 开一个带这组变量的终端。
- **不隔开。** 只有认这些变量的程序跟着走。Codex 自己连模型会走代理;agent 运行的命令里不认代理变量的(ssh、自己开连接的程序)
  会直接出去,Codex 启动的 MCP 服务也拿不到代理变量(它只传几个固定的变量,`TZ` 在里面);浏览器登录那一步用的是 Mac 上的浏览器。
  必须和这台 Mac 隔开的账号,用上面的笼子。

两种在界面里都是特殊环境:按 `^E` 单独列出(`n` 里没有),每个环境一块,标题写它在哪跑、时区和代理,
每个账号写着登录没有;列表里显示成 `team@work`、`team-unis@unis`。

在 `^E` 里选中账号按 `i` 登录:下方开一个终端,在它的环境里运行登录(笼子里的在笼子里,unis 的带着它的变量)。
已经登录的 Codex 账号会先问一句,因为 Codex 一开始登录就清掉原来的登录。登录链接弹出来时选「在别的电脑登录」:
链接复制好了,拿到别的电脑上打开、登录,浏览器最后会跳到一个打不开的 `http://localhost:1455/auth/callback?code=…`,
把这个地址粘贴回 aca,按 ⏎,aca 替那台浏览器把它送给这里等着的登录。`code` 几分钟就过期,要尽快贴回来。

## Antigravity 的 Google 登录和额度

装好 agy 后，账号页会显示 **Antigravity**。选中这一栏可以看当前 Google 登录的邮箱、最近记录的方案和额度使用情况。按 ⏎ 打开 agy 登录，按 `p` 编辑 `[tools.agy] args`。`aca account status` 也会显示这个登录。

额度只在开着不带账号的 `aca new agy` 会话时更新。账号页、`n` 菜单和计费页显示 Gemini 的 5 小时和 7 天额度已用百分比，用过第三方模型后还会显示 `3p 5h` 和 `3p 7d`。`aca limits --json` 和 `limits.changed` 中的 `used` 是已用百分比。

## Claude 的订阅额度和状态栏

Claude Code 只把订阅额度(5 小时和 7 天的额度各用了多少)提供给它的状态栏命令。
所以要让 aca 显示这些额度,Claude 的状态栏就得经过 aca。
这一步是可选的:不接入时其他功能不受影响,只是计费页顶部不显示 Claude 的额度。

```sh
aca setup                      # 一个页面:每个账号一行,用 ←→ 选择连接方式,回车生效
aca setup --statusline aca     # 不打开页面:所有 Claude 账号的状态栏都交给 aca 来画
aca setup --statusline wrap    # 不打开页面:保留你自己的状态栏,aca 只记录额度
aca setup --statusline off     # 撤销:恢复成连接之前的样子
```

- **aca**:由 aca 画一个两行的状态栏:上下文用量、模型、git 分支和改动、目录;
  第二行是这个账号的额度、花费和时长。(图标需要 Nerd Font 字体。)
- **wrap**:你的状态栏保持原样,只是在你的命令前面加上 `aca limits --`。

按模型分的周额度(比如 Fable)状态栏拿不到。对有会话在使用的账号,aca 每隔几分钟在后台向 Claude 查询一次
(与 `/usage` 同一来源)来补上这一项,只查询登记过的账号。

这个设置只修改一处:该账号 `settings.json` 里的 `statusLine` 这一项,文件的其他内容不变。
你原来的设置会被记下来,选 `off` 即可恢复。正在运行的 Claude 会话立即生效,无需重启。

如果你想自己写:

```json
"statusLine": { "type": "command", "command": "~/.local/bin/aca statusline", "refreshInterval": 10 }
```

<details>
<summary>实验功能:不经过状态栏也能看到额度</summary>

Claude Code 可以在启动时加载一小段代码。开启这个功能后,由 aca 启动的 Claude 会话不经过状态栏也能上报额度。
默认是关闭的。在界面里按 `Ctrl+S` 再按 `m` 可以开关,或者写在配置里:

```toml
[tools.claude]
mod = true
```

只有之后启动或重启的会话才会受影响。不会往你的配置目录里写任何东西。
这个功能依赖的 Claude Code 接口目前还标注为 early access,所以 aca 会对每个 Claude 版本先检查一次,
检查不通过就自动退回到只靠状态栏的方式。

</details>
