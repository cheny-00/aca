# aca 上手指南

[English](onboarding.md) | 简体中文

这一页带你从什么都没装,走到同时运行好几个 agent 会话。大约需要十五分钟。
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
(`ACA_LANG=zh` 是让安装脚本用中文提示,去掉就是英文。你的机器没有编译好的版本,或者想自己下载,
README 的[安装](../README.zh-CN.md#安装)一章里有另外两种装法。)完成后你应该看到:

```
装好了 aca 0.1.0 (ceaf172 2026-10-19):/Users/you/.local/bin/aca
下一步:
  aca setup    可以跳过。登记多出来的账号、选要不要让 aca 显示 Claude 的订阅额度;按 Esc 什么都不改
  aca          打开界面(按 n 新建会话,? 看快捷键)
```

脚本可能还会给出几条提醒:没有 tmux 或者版本太旧、`~/.local/bin` 不在 `PATH` 里、有些可选的工具没装。
前两条需要先解决再往下走。可选的工具可以以后再说,见[第 8 步](#8-可选的增强)。

如果脚本提示 `~/.local/bin` 不在 `PATH` 里,把下面这一行加到 `~/.zshrc`(或 `~/.bashrc`)里,然后重新打开终端:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

## 3. 启动第一个会话

```sh
aca
```

你会看到一个空的列表。界面默认是英文,想换成中文:按 `Ctrl+S`,再按 `l`。然后:

1. 按 `n`。弹出一个小菜单,列出可以启动的东西:shell、Claude、Codex。
2. 按对应的数字选择 Claude 或 Codex。
3. 输入一个项目的目录(按 Tab 可以补全路径),按 `回车`。

会话启动后,aca 直接带你进去。现在你就是在和 agent 对话,和你自己直接启动它完全一样。
以后在列表里选中一个会话按 `回车`,就能再进去。

### 第一次会被问到的两个问题

这两个问题是 agent 问的,不是 aca 问的。每个问题只需要处理一次。

**「是否信任这个目录里的文件?」** Claude 和 Codex 第一次在某个目录里运行时都会问。在会话里回答即可。
如果你希望 aca 每次都替你回答「是」,在界面里按 `Ctrl+S` 再按 `f`,或者在 `~/.config/aca/config.toml` 里写:

```toml
[trust]
folder = true
```

**「Hooks need review」(只有 Codex 会问)。** aca 是通过一种叫 hook 的小通知来了解会话正在做什么的
(在干活、在等你、做完了),这些 hook 由 aca 在启动 agent 时交给它。Codex 遇到没见过的 hook 会让你确认。
请选择 **Trust all**。Codex 会为这个账号记住你的选择,之后不再询问。
如果跳过这一步,Codex 会话照样能运行,但 aca 只能根据屏幕上的内容猜测它的状态,没法告诉你它什么时候做完了。

如果你完全不想被问:

```toml
[tools.codex]
hook_trust = "bypass"
```

需要注意:这样设置后,该会话里所有 Codex 还没审核过的 hook 都会直接运行,不只是 aca 的。

## 4. 回到列表

按 **`Ctrl+Q`**。你回到了 aca,而 agent 继续干活。

这是最需要记住的一个键。它在所有由 aca 启动的会话里都有效。
(在不是 aca 启动的 tmux 会话里,请用 tmux 自己的退出方式:先按前缀键,再按 `d`。)

## 5. 看懂列表

用 `n` 再启动两三个会话,给每个都安排点事情做,然后按 `Ctrl+Q` 回来。每个会话前面的标记表示它的状态:

| 标记 | 含义 | 你该做什么 |
|---|---|---|
| `●` | agent 正在干活 | 不用管,让它继续 |
| `◆` | 要你答:它提了问题,或者在等你批准权限 | 进去回答。按 `i` 可以直接跳到下一个要看的会话 |
| `◐` | 没看:它做完了,你还没看结果 | 进去看看 |
| `○` | 空闲,而且你已经看过它最后的回复 | |
| `✕` | 出了问题 | 进去看看 |
| `■` | 没在运行 | 按 `回车` 重新启动,并接着原来的对话 |

会话可以放进分组,比如每个项目一个分组。新建的会话会进入你当时选中的那个会话所在的分组;按 `m` 可以移动。

## 6. 最先该学的几个键

在界面里按 `?` 可以看到完整的列表。下面这些足够应付大多数时候:

| 按键 | 作用 |
|---|---|
| `↑` `↓` 或 `j` `k` | 在列表里上下移动 |
| `回车` | 进入会话(如果没在运行,会先启动它) |
| `Ctrl+Q` | 从会话回到列表 |
| `n` / `N` | 新建会话:先选工具 / 和当前选中的会话用同样的工具和目录 |
| `i` | 跳到下一个需要你处理的会话 |
| `z` | 不进入会话,全屏阅读它的输出 |
| `x` | 查看它改了什么(它工作目录的 git diff) |
| `Z` | 按轮次查看对话记录:执行了什么、改了什么、花了多少钱 |
| `r` / `R` / `D` | 改名 / 重启 / 停止 |
| `f` | 分叉:新开一个会话,从当前的对话接着往下走 |
| `d` | 删除会话(按 `Ctrl+Z` 可以撤销) |
| `/` | 过滤列表 |
| `$` | 用量和花费 |
| `Ctrl+S` | 设置:主题、语言、账号 |
| `q` `q` 或 `Esc` | 退出 aca。会话继续运行 |

README 的[按键](../README.zh-CN.md#按键)一节还列了更多的键、在会话里面能用的键,以及哪些键可以改。

## 7. 使用多个账号

如果你只有一个 Claude 登录和一个 Codex 登录,可以跳过这一节。

假如你有两个 Claude 订阅,一个个人的、一个公司的,它们各自有一个配置目录。给每个账号起个名字登记一下:

```sh
aca account add claude work ~/.claude-work
```

新建的目录需要登录一次,用工具自己的方式登录:

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

这一节的内容都是可选的,按需取用。

**查看 Claude 的订阅额度。** 运行 `aca setup`,移动到某个 Claude 账号,用 `←` `→` 选择状态栏的连接方式,按 `回车`。
`aca setup --statusline off` 可以撤销。各个选项的含义以及具体会改动什么,
见 [README](../README.zh-CN.md#claude-的订阅额度和状态栏)。

**在任何会话里都能打开 aca。** `aca tmux install` 会添加 tmux 快捷键,让你在当前会话上直接弹出 aca
(先按前缀键再按 `a`,或者直接按 `Ctrl+\`),并在 tmux 状态栏显示有几个会话在等你。
它会在你的 `tmux.conf` 里加两行;`aca tmux uninstall` 会把它们去掉。
即使什么都不装,在会话里按 `Ctrl+Alt+N` 也能直接跳到下一个在等你的会话。

**深色主题。** 默认是浅色主题。`aca theme use mocha` 切换到深色;`aca theme` 列出所有主题。

**中文界面。** 按 `Ctrl+S` 再按 `l`,或者设置环境变量 `ACA_LANG=zh`。

**zsh 的 Tab 补全。** `aca help completion` 里写了两种安装方法。

**额外的工具。** aca 可以打开 [yazi](https://github.com/sxyazi/yazi)(按 `y`)和
[lazygit](https://github.com/jesseduffield/lazygit)(在改动页面按 `l`),
并且会用 [fzf](https://github.com/junegunn/fzf) 和 [zoxide](https://github.com/ajeetdsouza/zoxide) 帮你挑选目录。
aca 不会替你安装这些工具,想用哪个请自己安装。完整的列表以及每个工具带来的功能,
见 [README](../README.zh-CN.md#可选的工具)。

**插件。** 在界面里按 `P` 查看有哪些可用。例如 `aca.notify` 会在某个会话做完或者需要你处理时发一条桌面通知。
详见 [plugins.zh-CN.md](plugins.zh-CN.md)。

## 9. 已经在运行的会话

如果你已经有手动在 tmux 里启动的 Claude 或 Codex,不需要把它们停掉。
它们会显示在列表里单独的一栏「其他 tmux 会话」中。选中一个,按 `A`;或者:

```sh
aca adopt                      # 列出可以接管的会话
aca adopt <tmux 会话名>
```

## 10. 用命令行操作会话

上面的所有操作都可以不通过界面完成。脚本和 agent 自己就是这样使用 aca 的:

```sh
id=$(aca new claude ~/code/app -t "修登录" --say '登录页返回 500,查一下原因')
aca wait "$id"                 # 等它做完这一轮、提出问题或者出错后返回
aca output "$id"               # 它最后的回复
aca send "$id" -m '再给它补一个测试'
```

`aca help` 列出了所有命令。

## 遇到问题时

| 现象 | 可能的原因和解决办法 |
|---|---|
| `aca: command not found` | `~/.local/bin` 不在 `PATH` 里。见第 2 步 |
| 新建的会话里是 agent 自己的欢迎页或登录页 | 它还没登录。就在这个会话里登录。`aca account status`(或者界面里 `Ctrl+S` 再按 `a`)能看到哪些登录着,不登记时用的 `~/.claude`、`~/.codex` 也在内 |
| 新建的会话马上就显示 `■` | agent 一启动就退出了。在那个目录里单独运行 `claude`(或 `codex`),按它的提示解决 |
| Codex 会话做完了,却从来不显示 `◐`(做完了还没看) | 它的 hook 没有被批准。重启它(按 `R`),在 Codex 询问时选择「Trust all」。见第 3 步 |
| 按 `Ctrl+Q` 没反应 | 你所在的 tmux 会话不是 aca 启动的:请先按 tmux 前缀键再按 `d`。或者你的终端或 tmux 配置已经占用了 `Ctrl+Q` |
| Claude 的状态栏里图标显示成方块或问号 | aca 画的状态栏用了 Nerd Font 图标。安装一个 [Nerd Font](https://www.nerdfonts.com) 字体,或者在 `aca setup` 里选 `wrap`,保留你自己的状态栏 |
| 颜色看不清 | 换一个主题:`aca theme use mocha`(深色)或 `aca theme use latte`(浅色) |
| 界面的语言不对 | 按 `Ctrl+S` 再按 `l` |

还是解决不了?`aca help <命令>` 和 `aca help keys` 里有每个功能的详细说明,也可以在 GitHub 上提 issue。

## 升级和卸载

再运行一遍安装命令就是升级。要彻底删除 aca,请按 [README 的「卸载」一节](../README.zh-CN.md#卸载)操作。
