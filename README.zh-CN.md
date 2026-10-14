# aca

**在一个屏幕上管理你所有的 AI 编程助手会话** —— Claude Code、Codex,以及你自己接进来的其他 agent。

[English](README.md) | 简体中文

<!-- TODO: 这里放一张界面的截图或者一小段 GIF -->

同时开着好几个 agent 的时候,很难分清哪个还在干活、哪个停下来在等你回答、哪个在你没注意的时候已经做完了。
aca 把所有会话放进一个列表,选中哪个就显示它的实时画面,按回车就进去。

每个会话都是一个普通的 tmux 会话。关掉 aca,agent 照常运行;你也可以直接用 tmux 进去,
包括从另一台电脑或者手机上通过 SSH 连进去。

## 它能做什么

- **一个列表,状态一眼看清。** `●` 正在干活 · `◐` 在等你,或者做完了你还没看 · `○` 空闲 · `✕` 出错 · `■` 没在运行。
- **多个账号放在一起用。** 个人订阅、公司订阅、API key、Bedrock:所有账号的会话都在同一个列表里,
  而且 aca 保证一个账号的登录不会被另一个账号的会话用到。
- **不改动你的 agent 配置。** aca 不会往 `~/.claude` 或 `~/.codex` 里写 hook 或者别的东西。
  它需要的东西是在启动会话时通过命令行参数带进去的。登录永远用工具自己的登录命令。
- **停下后能在原处继续。** 把一个会话停掉,以后再启动:它会在同一个 tmux 窗格里接着原来的对话。
  你在旁边另开的窗口不受影响。
- **不进会话也能看。** 在列表里就能看一个会话的输出、它改了哪些代码(git diff),
  或者按轮次看对话记录(每一轮执行了什么、改了哪些文件、花了多少钱)。
- **界面能做的事,命令行都能做。** `aca new`、`aca send`、`aca wait`、`aca answer` 等命令和界面的功能一一对应,
  所以脚本可以操作会话,一个 agent 也可以启动另一个 agent、给它派任务、在它做完时收到通知。
- **接管已经在运行的会话。** 你手动在 tmux 里启动的 Claude 或 Codex,不用停下就能收进列表。
- **用量和花费。** 本机的 Claude 和 Codex 用量按 API 价格折算是多少钱,以及订阅额度已经用了多少。
- **插件。** 一个插件就是一个目录加一份清单文件,可以添加命令、响应事件、打开窗格,
  还可以给 agent 带上 skill 和 MCP server。aca 自带几个:桌面通知、把任务派给另一个会话、
  把耗时长的任务放到另一台机器上跑,等等。
- **没有后台常驻进程。** 状态就是 agent 的 hook 写下的几个小文件。你不用它的时候,什么都不在运行。

## 需要什么

- macOS 或 Linux。aca 是在 macOS 上开发和日常使用的。Linux 上能编译、基本功能可用,
  但测试少得多,插件的定时任务目前只支持 macOS。
- [tmux](https://github.com/tmux/tmux) 3.3 或更新的版本
- [Claude Code](https://github.com/anthropics/claude-code) 和 / 或 [Codex CLI](https://github.com/openai/codex),
  已经按你平时的方式装好并登录

## 安装

```sh
curl -fsSL https://raw.githubusercontent.com/cheny-00/aca/main/install.sh | sh
```

这条命令下载一个已经编译好的文件,放到 `~/.local/bin/aca`。它只做这一件事:

- **不安装任何别的东西**:不装 tmux,不装 Rust,也不装 fzf、yazi、lazygit 等工具。
  如果缺了 aca 需要的东西,脚本会告诉你,由你自己决定装不装。
- **不修改**你的 shell 启动文件、`tmux.conf`,也不修改 Claude / Codex 的配置。
- 安装之前会用发布时公布的 SHA-256 校验和核对下载的文件。

安装脚本的提示默认是英文;想看中文,把命令最后的 `sh` 换成 `ACA_LANG=zh sh`。

参数写在 `sh -s --` 后面:

```sh
curl -fsSL https://raw.githubusercontent.com/cheny-00/aca/main/install.sh | sh -s -- --prefix /usr/local
```

| 参数 | 作用 |
|---|---|
| `--prefix <目录>` | 安装到 `<目录>/bin/aca`(默认是 `~/.local`) |
| `--version <tag>` | 安装指定的版本,例如 `v0.1.0`(默认是最新版) |
| `--from-source` | 不下载,用 cargo 从源码编译(需要 Rust 1.89 以上) |
| `--statusline <方式>` | 安装后顺便把 Claude 的状态栏接上:`aca`、`wrap` 或 `off`(含义见[下文](#claude-的订阅额度和状态栏)) |
| `--no-setup` | 只安装程序,不提示后续步骤 |

如果 `~/.local/bin` 不在你的 `PATH` 里,脚本会提醒你;请在 shell 的启动文件里把它加上。

编译好的版本支持 macOS(Apple 芯片和 Intel)和 Linux(x86_64 和 arm64,glibc 2.35 以上)。
其他环境请从源码编译:

```sh
git clone https://github.com/cheny-00/aca && cd aca
./install.sh              # 需要 Rust 1.89 以上;编译后安装到 ~/.local/bin/aca
```

**升级:** 把安装命令再运行一遍。你的设置和会话都不会变。

## 快速上手

```sh
aca setup                 # 可以跳过,说明见下
aca                       # 打开界面
```

1. 在界面里按 `n`,选 Claude 或 Codex,确认目录。一个会话就启动了。
2. 按 `回车` 进入会话,像平时一样和 agent 对话。
3. 按 `Ctrl+Q` 回到列表。agent 会继续干活。
4. 再多开几个。留意列表里每个会话前面的标记;按 `i` 可以跳到下一个需要你处理的会话。
5. 随时按 `?` 查看快捷键。

`aca setup` 是一个设置页面,可以登记额外的账号、选择要不要让 aca 显示 Claude 的订阅额度。
按回车之前什么都不会改,按 Esc 则一切保持原样。如果你只用默认的 `~/.claude` 和 `~/.codex`,可以跳过这一步。

界面默认是英文。想用中文:在界面里按 `Ctrl+S` 再按 `l`,或者设置环境变量 `ACA_LANG=zh`。

**第一次用?[上手指南](docs/onboarding.zh-CN.md)带你走完最初的十五分钟**,
包括每个 agent 第一次启动时会问的两个问题,以及怎样让它们以后不再问。

## 常用命令

界面里能做的事都有对应的命令。会话的 id 只写开头几位就行,也可以直接写会话的标题。

```sh
aca new claude ~/code/app                 # 在某个目录里启动一个 Claude 会话
aca new codex . -t "修登录" -g work       # 指定标题,并放进一个分组
aca new claude:work . --say '先读一下 README'    # 使用名为 work 的账号,并带上第一句话
aca ls                                    # 列出所有会话
aca attach 3f2a                           # 进入会话(Ctrl+Q 回来)
aca send 3f2a -m '接着做第二步'           # 往会话里输入一句话并回车
aca wait 3f2a                             # 一直等到它做完这一轮、提出问题或者出错
aca answer 3f2a 2                         # 替它选第 2 个选项
aca interrupt 3f2a                        # 它做偏了:中止当前这一轮
aca stop 3f2a && aca start 3f2a           # 停下,然后在原处接着原来的对话启动
aca fork 3f2a -t "试试另一个方案"         # 从它当前的对话分出一个新会话
aca rename 3f2a "登录页改版"              # tmux 会话也会一起改名
aca cost                                  # 用量和花费
aca help                                  # 一屏列出所有命令;aca help <命令> 查看某个命令的详细说明
```

在终端里,输出是排好版、带颜色的;通过管道交给别的程序时是纯文本。
`ls`、`show`、`new`、`adopt` 加上 `--json` 可以输出给程序读的格式。

## 配置

所有配置都是可选的。没有配置文件 aca 也能正常使用。

`~/.config/aca/config.toml`:

```toml
[tools.claude]
account = "work"                          # 运行 aca new claude 而没有指定账号时,用哪个账号
args = ["--model", "opus"]                # 每次启动都加上的参数

[tools.codex]
hook_trust = "bypass"                     # 不再询问「Hooks need review」(见上手指南)

[trust]
folder = true                             # 替你回答「是否信任这个目录」

[ui]
lang = "zh"                               # "en" 或 "zh"

[tools.aider]                             # 接入别的 agent:只需声明,不用写代码
resume = ["--session", "{conv}"]
busy = ['Working\.\.\.']
```

`aca help config` 列出了全部配置项。界面相关的设置(主题、语言等)大多可以在设置页(`Ctrl+S`)里改,
改完会自动保存到这个文件。

内置支持的是 Claude、Codex 和普通的 shell。其他 agent 可以像上面的 `[tools.aider]` 那样直接在这个文件里声明,不用写代码。

## 账号

一个账号其实就是**「以这个身份启动工具时,你需要额外输入的那些东西」**:
换一个配置目录、设几个环境变量、加几个参数。你只需要写一次,之后每次启动 aca 都会替你加上。

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
- **工具自己的命令**(登录、管理 MCP 等)通过 `aca run` 来运行:`aca run claude:work -- mcp list`。
  它会带上账号的 `home`、`env` 和 `env_file`,但不带 `args`,因为 `args` 只用于启动会话。
- **你自己的 `--settings` 文件会被保留。** Claude 只认它收到的最后一个 `--settings`,
  所以如果你传了自己的,aca 会把它的 hook 合并进你那份文件的副本,再把副本传给 Claude。你的原文件不会被修改。
- **删除账号**(`aca account rm claude work`)只是去掉 `config.toml` 里的那一行。
  配置目录和里面的登录都不会动。
- **重新登录会覆盖当前的登录。** 工具的登录命令一启动,就会丢弃现有的登录
  (`codex login` 会立刻删除 `auth.json`)。对已经登录的账号,账号页面会先向你确认。
- aca 从不保存或复制凭据,也从不往这些配置目录里写东西。唯一的例外需要你主动选择:下面说的状态栏设置。

## Claude 的订阅额度和状态栏

Claude Code 只把订阅额度(5 小时和 7 天的额度各用了多少)提供给它的状态栏命令。
所以要让 aca 显示这些额度,Claude 的状态栏就得经过 aca。
这是可选的:不接的话其他功能都照常,只是花费页面顶部没有 Claude 的额度。

```sh
aca setup                      # 一个页面:每个账号一行,用 ←→ 选择连接方式,回车生效
aca setup --statusline aca     # 不打开页面:所有 Claude 账号的状态栏都交给 aca 来画
aca setup --statusline wrap    # 不打开页面:保留你自己的状态栏,aca 只记录额度
aca setup --statusline off     # 撤销:恢复成连接之前的样子
```

- **aca**:由 aca 画一个两行的状态栏:上下文用量、模型、git 分支和改动、目录;
  第二行是这个账号的额度、花费和时长。(图标需要 Nerd Font 字体。)
- **wrap**:你的状态栏保持原样,只是在你的命令前面加上 `aca limits --`。

这个设置只改一处:该账号 `settings.json` 里的 `statusLine` 这一项。文件的其他内容不会动,
你原来的设置会被记下来,选 `off` 就恢复。正在运行的 Claude 会话会马上生效,不用重启。

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

## 不是用 aca 启动的会话

不是 aca 启动的会话(比如你手动在 tmux 里运行的 `claude`)会出现在界面的「其他 tmux 会话」里。
你可以查看、进入它们,但不能改名、重启或分叉。要把它交给 aca 管理,而且**不用停下它**:

```sh
aca adopt                      # 列出可以接管的会话
aca adopt my-session           # 接管其中一个(界面里按 A)
aca adopt --all
aca release <id>               # 反过来:取消登记,进程继续运行
```

aca 会判断这个会话用的是哪个工具、哪个账号,以及它正在进行哪一段对话。
在第一次重启之前,接管来的会话没有 aca 的 hook,所以它的状态只能靠读屏幕来判断,
也不会有「做完了还没看」这个状态。运行一次 `aca restart <id>`(会接着原来的对话),之后它就和其他会话一样了。

之前用 [agent-deck](https://github.com/asheshgoplani/agent-deck)?`aca import agent-deck` 会列出它的分组和会话,
加上 `--apply` 才真正导入。还在那边运行着的会话,建议用上面的接管方式。

## 可选的工具

下面这些 aca 都不会替你安装,一个都不装 aca 也能用。每一个只是开启它旁边写的那个功能。
想用哪个,就用你的包管理器自己装哪个。

| 工具 | 带来的功能 |
|---|---|
| `git` | 查看会话的改动(`x`)、在 worktree 里启动会话(`aca new -w`)、从代码仓库安装插件 |
| [fzf](https://github.com/junegunn/fzf) | 新建会话时模糊搜索目录(`↓` 或 `Ctrl+F`);`aca.copy` 插件 |
| [zoxide](https://github.com/ajeetdsouza/zoxide) | 新建会话时,候选目录里会出现你常去的目录 |
| [yazi](https://github.com/sxyazi/yazi) | 按 `y` 在会话当前工作的目录里打开文件管理器 |
| [lazygit](https://github.com/jesseduffield/lazygit) | 在改动页面按 `l`,在那个仓库里打开 lazygit |
| `jq` | 大多数自带插件需要它(通知、派任务、远程任务) |
| [Nerd Font](https://www.nerdfonts.com) 字体 | aca 为 Claude 画的状态栏里的图标 |
| `$EDITOR` | 按 `e` 打开你正在看的文件或记录 |

另外还有两样可以自己开启,aca 不会替你做:

```sh
aca tmux install               # 添加 tmux 快捷键,可以在任何会话里把 aca 弹出来;
                               # 并在 tmux 状态栏显示有几个会话在等你。会在你的 tmux.conf 里加两行;
                               # aca tmux uninstall 会把它们去掉
aca completion zsh             # 输出 zsh 的补全脚本;放在哪里见 aca help completion
```

## aca 的文件放在哪里

| | |
|---|---|
| 配置 | `~/.config/aca/`(`config.toml`,以及插件的选择和设置) |
| 会话和插件 | `~/.local/share/aca/`(`aca.db`、已安装的插件) |
| 运行状态 | `~/.local/state/aca/`(hook 写下的会话状态、事件日志、收件箱、日志,以及置顶、便签等界面状态) |
| 缓存 | `~/.cache/aca/` |

除此之外,aca 只是调用 tmux、启动你的 agent。只有在你明确要求时,它才会修改其他位置的文件:
某个 Claude 账号 `settings.json` 里的 `statusLine`(`aca setup`)、你的 `tmux.conf` 里的两行(`aca tmux install`)、
给项目挂载 MCP server 时该项目的 `.mcp.json`(`M`),以及在 macOS 上你开启某个插件定时任务时的一条 launchd 记录。

## 卸载

```sh
aca setup --statusline off     # 只有接过 Claude 状态栏才需要:恢复你原来的状态栏
aca tmux uninstall             # 只有运行过 aca tmux install 才需要
aca plugin timer               # 列出插件的定时任务;把开着的关掉:aca plugin timer off <插件.命令>
rm ~/.local/bin/aca
rm -rf ~/.config/aca ~/.local/share/aca ~/.local/state/aca ~/.cache/aca
```

以上操作都不会关闭你的 tmux 会话和里面的 agent。由 aca 启动的会话会在 hook 里调用 `aca` 这个程序,
所以删除之后请关闭或重启那些 agent。你和 Claude、Codex 的对话保存在它们各自的目录里,不受影响。

## 文档

- [上手指南](docs/onboarding.zh-CN.md):最初的十五分钟
- `aca help`、`aca help <命令>`、`aca help keys`、`aca help config`:完整的参考,内容始终和你安装的版本一致
- [docs/plugins.md](docs/plugins.md):使用和编写插件
- [hub/README.md](hub/README.md):远程任务插件背后的小型服务端

## 开发

```sh
cargo test                               # 单元测试和端到端测试(需要 tmux;测试会自己启动一个隔离的 tmux 服务)
cargo clippy --all-targets && cargo fmt
./install.sh                             # 编译当前这份代码并安装
```

代码分成三个 crate:`crates/aca-core`(会话后端,是一个库)、`crates/aca-tui`(界面,只依赖 core)、
`crates/aca`(命令行,也是界面的入口)。自带的插件在 `plugins/` 下,会被编译进程序里。
每条命令的用法和帮助文字都写在 `crates/aca/src/help.rs` 的一张表里;
界面的按键说明在 `crates/aca-tui/src/keys.txt` 和 `keys.en.txt` 里。

**手动测试时请做好隔离。** 设置 `ACA_HOME=<一个临时目录>`,配置、会话数据库和状态就都放在那里,不会碰到你真实的数据;
设置 `ACA_TMUX_SOCKET=<一个名字>` 则使用一个单独的 tmux 服务。
`aca ui --snapshot 100x30 [按键]` 可以把界面的一帧画成文字输出,不会真的执行任何操作。
如果你在某个 agent 会话里手动启动测试用的 agent,请先清除外层会话的环境变量(所有以 `CLAUDE` 开头的):
tmux 服务会把自己启动时的环境变量传给每一个会话,其中 `CLAUDE_SECURESTORAGE_CONFIG_DIR`
会让里面启动的 Claude 使用外层账号的登录。aca 自己启动 agent 时会去掉这些变量,但手写的测试脚本不会。

发布新版本:推送一个形如 `v0.1.0` 的 tag。`.github/workflows/release.yml` 里的流程会编译出安装脚本要下载的那些文件。
