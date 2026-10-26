# aca 上手指南

[English](onboarding.md) | 简体中文

这一页带你从零开始,到同时运行好几个 agent 会话,大约需要十五分钟。
每一步都写了要输入什么,以及你应该看到什么。

## 1. 先确认需要的东西

aca 管理的是你已经在用的 agent 的会话。它不会替你安装这些 agent,也不会替你登录。

| 需要 | 怎么确认 | 没有的话 |
|---|---|---|
| macOS 或 Linux | | |
| tmux 3.3 或更新 | `tmux -V` | `brew install tmux`,或者用 Linux 发行版的包管理器安装 |
| Claude Code 和 / 或 Codex CLI,并且已登录 | 运行一次 `claude` 或 `codex`,确认它能正常回答 | 按平时的方式安装并登录 |

你不需要安装 Rust,也不需要会用 tmux。tmux 由 aca 替你操作。

## 2. 安装

```sh
curl -fsSL https://raw.githubusercontent.com/cheny-00/aca/main/install.sh | ACA_LANG=zh sh
```

这条命令只会在 `~/.local/bin/aca` 放一个文件,不安装任何别的东西。
(`ACA_LANG=zh` 让安装脚本用中文提示,去掉则是英文。如果你的机器没有编译好的版本,或者想自己下载,
README 的[安装](../README.zh-CN.md#安装)一章里有另外两种装法。)完成后你应该看到:

```
已安装 aca 0.1.0 (ceaf172 2026-10-19):/Users/you/.local/bin/aca
下一步:
  aca setup    可跳过。添加其他账号,选择是否让 aca 显示 Claude 的订阅额度;按 Esc 不做任何修改
  aca          打开界面(按 n 新建会话,按 ? 查看快捷键)
```

脚本可能还会给出几条「注意」:没有 tmux 或版本太旧、`~/.local/bin` 不在 `PATH` 中、有些可选工具未安装。
前两条需要先解决再继续。可选工具可以以后再装,见[第 8 步](#8-可选的增强)。

如果脚本提示 `~/.local/bin` 不在 `PATH` 中,把下面这一行加到 `~/.zshrc`(或 `~/.bashrc`)里,然后重新打开终端:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

## 3. 启动第一个会话

```sh
aca
```

你会看到一个空的列表。界面默认是英文,想换成中文:按 `Ctrl+S`,再按 `l`。然后:

1. 按 `n`,弹出新建菜单,列出可以启动的工具:shell、Claude、Codex。
2. 按对应的数字选择 Claude 或 Codex。
3. 输入一个项目的目录(按 Tab 可以补全路径),按 `回车`。

会话启动后,aca 会直接进入这个会话。现在你就是在和 agent 对话,和你直接启动它完全一样。
以后在列表里选中一个会话按 `回车`,就能再次进入。

### 第一次会被问到的两个问题

这两个问题是 agent 问的,不是 aca 问的。每个问题只需要处理一次。

**「是否信任这个目录里的文件?」** Claude 和 Codex 第一次在某个目录里运行时都会问。在会话里回答即可。
如果你希望 aca 每次都替你回答「是」,在界面里按 `Ctrl+S` 再按 `f`,或者在 `~/.config/aca/config.toml` 里写:

```toml
[trust]
folder = true
```

**「Hooks need review」(只有 Codex 会问)。** aca 通过 hook(agent 在关键时刻发出的通知)了解会话的状态
(运行中、等待回应、已完成),这些 hook 由 aca 在启动 agent 时交给它。Codex 遇到没见过的 hook 会让你确认。
请选择 **Trust all**。Codex 会为这个账号记住你的选择,之后不再询问。
如果跳过这一步,Codex 会话照样能运行,但 aca 只能根据屏幕上的内容推测它的状态,无法告诉你它何时完成。

如果你完全不想被问:

```toml
[tools.codex]
hook_trust = "bypass"
```

注意:这样设置后,该会话里所有 Codex 尚未审核的 hook 都会直接运行,不只是 aca 的。

## 4. 回到列表

按 **`Ctrl+Q`** 回到 aca 的列表,agent 继续运行。

这是最需要记住的一个键。它在所有由 aca 启动的会话里都有效。
(在不是 aca 启动的 tmux 会话里,请用 tmux 自己的退出方式:先按前缀键,再按 `d`。)

## 5. 看懂列表

用 `n` 再新建两三个会话,给每个都安排一些任务,然后按 `Ctrl+Q` 返回。每个会话前面的标记表示它的状态:

| 标记 | 含义 | 你该做什么 |
|---|---|---|
| `●` | 运行中 | 等待完成 |
| `◆` | 等待回应:有问题或权限请求需要你处理 | 进去回答。按 `i` 可以直接跳到下一个待处理会话 |
| `◐` | 未读:任务已完成,尚未查看结果 | 查看结果 |
| `○` | 空闲,而且你已经看过它最后的回复 | |
| `✕` | 出错 | 查看错误信息 |
| `■` | 已停止 | 按 `回车` 重新启动,继续原来的对话 |
| `☾` | 自动停止:空闲太久,已被 aca 停止(在 `config.toml` 的 `[idle]` 中设置) | 按 `回车` 重新启动,继续原来的对话 |

会话可以放进分组,比如每个项目一个分组。新建的会话会进入当时选中的会话所在的分组;按 `m` 可以移到别的分组。

## 6. 最先该学的几个键

在界面里按 `?` 可以看到完整的列表。日常使用,下面这些就够了:

| 按键 | 作用 |
|---|---|
| `↑` `↓` 或 `j` `k` | 在列表里上下移动 |
| `回车` | 进入会话(未运行的会先启动) |
| `Ctrl+Q` | 从会话回到列表 |
| `n` / `N` | 新建 / 按当前配置新建(沿用工具、账号和目录) |
| `i` | 打开下一个待处理的会话 |
| `z` | 全屏查看输出,不进入会话 |
| `x` | 查看改动(会话工作目录的 git diff) |
| `Z` | 按轮次查看对话记录:执行了什么、改了什么、花费多少 |
| `r` / `R` / `D` | 重命名 / 重启 / 停止 |
| `f` | 分叉:新建一个会话,从当前对话继续 |
| `d` | 删除会话(按 `Ctrl+Z` 可以撤销) |
| `/` | 过滤列表 |
| `$` | 用量和花费 |
| `Ctrl+S` | 设置:主题、语言、账号 |
| `q` `q` | 退出 aca,会话继续运行 |

README 的[按键](../README.zh-CN.md#按键)一节还列了更多的键、在会话里面能用的键,以及哪些键可以改。

## 7. 使用多个账号

如果你只有一个 Claude 登录和一个 Codex 登录,可以跳过这一节。

假如你有两个 Claude 订阅,一个个人的、一个公司的,各自有一个配置目录。给每个账号起个名字并登记:

```sh
aca account add claude work ~/.claude-work
```

新的配置目录需要登录一次,用工具自己的方式登录:

```sh
aca run claude:work            # 用这个账号启动 Claude,让你完成登录
aca run codex:work -- login    # Codex 的登录
```

之后 `n` 的菜单里会列出你的账号;在命令行里则写成 `claude:work`:

```sh
aca new claude:work ~/code/app
```

`aca setup` 用一个页面展示同样的内容(按 `+` 添加账号);在界面里按 `Ctrl+S` 再按 `a` 可以打开账号页面,
那里还能看到每个账号是否已登录。更多细节见 [README 的「账号」一节](../README.zh-CN.md#账号)。

## 8. 可选的增强

这一节的内容都是可选的,按需使用。

**查看 Claude 的订阅额度。** 运行 `aca setup`,移动到某个 Claude 账号,用 `←` `→` 选择状态栏的连接方式,按 `回车`。
`aca setup --statusline off` 可以撤销。各个选项的含义以及具体会改动什么,
见 [README](../README.zh-CN.md#claude-的订阅额度和状态栏)。

**在任何会话里都能打开 aca。** `aca tmux install` 会添加 tmux 快捷键,让你在当前会话上直接弹出 aca
(先按前缀键再按 `a`,或者直接按 `Ctrl+\`),并在 tmux 状态栏显示待处理会话的数量。
它会在你的 `tmux.conf` 里加两行;`aca tmux uninstall` 会移除这两行。
即使不运行这条命令,在会话里按 `Ctrl+Alt+N` 也能直接进入下一个待处理的会话。

**深色主题。** 默认是浅色主题。`aca theme use mocha` 切换到深色;`aca theme` 列出所有主题。

**中文界面。** 按 `Ctrl+S` 再按 `l`,或者设置环境变量 `ACA_LANG=zh`。

**zsh 的 Tab 补全。** `aca help completion` 里写了两种安装方法。

**额外的工具。** aca 可以打开 [yazi](https://github.com/sxyazi/yazi)(按 `y`)和
[lazygit](https://github.com/jesseduffield/lazygit)(在改动页面按 `l`),
并且会用 [fzf](https://github.com/junegunn/fzf) 和 [zoxide](https://github.com/ajeetdsouza/zoxide) 帮你选择目录。
aca 不会替你安装这些工具,需要哪个请自行安装。完整的列表以及每个工具带来的功能,
见 [README](../README.zh-CN.md#可选的工具)。

**插件。** 在界面里按 `P` 查看有哪些插件。例如 `aca.notify` 会在会话完成或等待回应时发送桌面通知。
详见 [plugins.zh-CN.md](plugins.zh-CN.md)。

## 9. 已经在运行的会话

如果你已经有手动在 tmux 里启动的 Claude 或 Codex,不需要把它们停掉。
它们会显示在列表底部的「其他 tmux 会话」分区中。选中一个按 `A`,或者:

```sh
aca adopt                      # 列出可以接管的会话
aca adopt <tmux 会话名>
```

## 10. 用命令行操作会话

上面的所有操作都可以不通过界面完成,脚本和 agent 就是这样使用 aca 的:

```sh
id=$(aca new claude ~/code/app -t "修登录" --say '登录页返回 500,查一下原因')
aca wait "$id"                 # 等它完成这一轮、提出问题或出错后返回
aca output "$id"               # 它最后的回复
aca send "$id" -m '再给它补一个测试'
```

`aca help` 列出了所有命令。

## 遇到问题时

| 现象 | 可能的原因和解决办法 |
|---|---|
| `aca: command not found` | `~/.local/bin` 不在 `PATH` 里。见第 2 步 |
| 新建的会话里是 agent 自己的欢迎页或登录页 | 该账号未登录,直接在这个会话里登录即可。`aca account status`(或在界面里按 `Ctrl+S` 再按 `a`)可以查看各账号是否已登录,包括未登记时使用的 `~/.claude` 和 `~/.codex` |
| 新建的会话马上显示 `■` | agent 一启动就退出了。在那个目录里单独运行 `claude`(或 `codex`),按它的提示解决 |
| Codex 会话已完成,却从不显示 `◐`(未读) | 它的 hook 未被批准。重启它(按 `R`),在 Codex 询问时选择「Trust all」。见第 3 步 |
| 按 `Ctrl+Q` 没有反应 | 你所在的 tmux 会话不是 aca 启动的:请先按 tmux 前缀键再按 `d`。或者你的终端或 tmux 配置已经占用了 `Ctrl+Q` |
| Claude 的状态栏里图标显示成方块或问号 | aca 画的状态栏用了 Nerd Font 图标。安装一个 [Nerd Font](https://www.nerdfonts.com) 字体,或者在 `aca setup` 里选 `wrap`,保留你自己的状态栏 |
| 颜色看不清 | 换一个主题:`aca theme use mocha`(深色)或 `aca theme use latte`(浅色) |
| 界面语言不对 | 按 `Ctrl+S` 再按 `l` |

仍未解决?`aca help <命令>` 和 `aca help keys` 里有每个功能的详细说明,也可以在 GitHub 上提 issue。

## 升级和卸载

再运行一遍安装命令即可升级。要彻底删除 aca,请按 [README 的「卸载」一节](../README.zh-CN.md#卸载)操作。
