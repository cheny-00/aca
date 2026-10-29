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
aca account status                        # 各个账号是否已登录(是向工具本身查询的,aca 从不读取凭据)
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
