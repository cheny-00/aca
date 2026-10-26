# aca

**在一个屏幕上管理你所有的 AI 编程助手会话** —— Claude Code、Codex、pi、opencode,以及你自己接进来的其他 agent。

[English](README.md) | 简体中文

<!-- TODO: 这里放一张界面的截图或者一小段 GIF -->

同时运行好几个 agent 时,很难分清哪个还在工作、哪个在等你回答、哪个已经在你没注意时完成了。
aca 把所有会话放进一个列表,选中哪个就显示它的实时画面,按回车进入。

每个会话都是一个普通的 tmux 会话。关闭 aca 后 agent 照常运行;你也可以直接用 tmux 进入会话,
包括从另一台电脑或手机上通过 SSH 连接。

aca 是参考 [agent-deck](https://github.com/asheshgoplani/agent-deck) 做的,见[致谢](#致谢)。

## 它能做什么

- **一个列表,状态一眼看清。** `●` 运行中 · `◆` 等待回应 · `◐` 未读 · `○` 空闲 · `✕` 出错 · `■` 已停止 · `☾` 自动停止(见下面的 `[idle]`)。
- **多个账号放在一起用。** 个人订阅、公司订阅、API key、Bedrock:所有账号的会话都在同一个列表里,
  而且 aca 保证一个账号的登录不会被另一个账号的会话用到。
- **不改动你的 agent 配置。** aca 不会往 `~/.claude` 或 `~/.codex` 里写 hook 或者别的东西。
  它需要的东西是在启动会话时通过命令行参数带进去的。登录永远用工具自己的登录命令。
- **停止后能在原处继续。** 停止一个会话,之后再启动时,它会在同一个 tmux 窗格里继续原来的对话。
  你在会话里另开的窗口不受影响。
- **不进会话也能看。** 在列表里就能看一个会话的输出、它改了哪些代码(git diff),
  或者按轮次看对话记录(每一轮执行了什么、改了哪些文件、花了多少钱)。
- **界面能做的事,命令行都能做。** `aca new`、`aca send`、`aca wait`、`aca answer` 等命令和界面的功能一一对应,
  所以脚本可以操作会话,一个 agent 也可以启动另一个 agent、给它派任务、在它做完时收到通知。
- **接管已经在运行的会话。** 你手动在 tmux 里启动的 Claude 或 Codex,无需停止就能加入列表。
- **用量和花费。** 本机的 Claude 和 Codex 用量按 API 价格折算是多少钱,以及订阅额度已经用了多少。
- **插件。** 一个插件就是一个目录加一份清单文件,可以添加命令、响应事件、打开窗格,
  还可以给 agent 带上 skill 和 MCP server。aca 自带几个:桌面通知、把任务派给另一个会话、
  把耗时长的任务放到另一台机器上运行,等等。
- **没有后台常驻进程。** 状态就是 agent 的 hook 写下的几个小文件。你不用它的时候,什么都不在运行。

## 安装

aca 就是一个程序 `aca`。有三种装法,都是把它放到 `~/.local/bin/aca`(可以用 `--prefix` 换位置),不安装任何别的东西。

| 装法 | 适合 | 需要 |
|---|---|---|
| [安装脚本](#安装脚本) | x86_64 / arm64 的 macOS 或 Linux:大多数人用这个 | `curl` 或 `wget` |
| [手动下载](#手动下载) | 想每一步自己来 | `tar`、`shasum` 或 `sha256sum` |
| [从源码编译](#从源码编译) | 你的机器没有编译好的版本,或者想用最新的代码 | Rust 1.89 以上、C 编译器、git |

### 安装之前

不管用哪种装法,aca 运行都需要下面这些。安装脚本会检查它们、告诉你缺了什么,但不会替你安装。

- **macOS 或 Linux。** aca 是在 macOS 上开发和日常使用的。Linux 上能编译、基本功能可用,但测试少得多,
  插件的定时任务目前只支持 macOS。编译好的版本支持 macOS(Apple 芯片和 Intel)和 Linux(x86_64 和 arm64),
  Linux 要求 glibc 2.35 以上,也就是 Ubuntu 22.04、Debian 12 或更新的版本。`uname -sm` 可以看你的机器是哪种。
- **[tmux](https://github.com/tmux/tmux) 3.3 或更新的版本。** aca 启动的每个会话都是一个 tmux 会话;用 aca 不需要会用 tmux。
  用 `tmux -V` 查看版本。macOS 上 `brew install tmux`;Linux 上用包管理器安装,但要注意 Ubuntu 22.04 自带的
  tmux 是 3.2a,版本不够;Ubuntu 24.04、Debian 12 的就够了。
- **[Claude Code](https://github.com/anthropics/claude-code)、[Codex CLI](https://github.com/openai/codex)、
  [pi](https://pi.dev) 或 [opencode](https://opencode.ai)(有一个就行),** 已经按你平时的方式装好并登录。运行一次 `claude`、`codex`、`pi`
  或 `opencode`,确认它能正常回答。aca 会启动它们,但不会替你安装,也不会替你登录。pi 和 opencode 要能在 `PATH` 中找到,才会出现在新建会话的菜单里。
- **`~/.local/bin` 在你的 `PATH` 里**(或者你安装到的那个目录)。不在的话,把
  `export PATH="$HOME/.local/bin:$PATH"` 加到 `~/.zshrc` 或 `~/.bashrc`,再开一个新终端。

git、fzf、jq 这些工具是可选的,每个只开启一项功能,见[可选的工具](#可选的工具)。

### 安装脚本

```sh
curl -fsSL https://raw.githubusercontent.com/cheny-00/aca/main/install.sh | sh
```

这条命令下载一个已经编译好的文件,放到 `~/.local/bin/aca`。它只做这一件事:

- **不安装任何别的东西**:不装 tmux,不装 Rust,也不装 fzf、yazi、lazygit 等工具。
  如果缺少 aca 需要的东西,脚本会提示你,由你决定是否安装。
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
| `--from-source` | 不下载,用 cargo 从源码编译(需要什么见[从源码编译](#从源码编译)) |
| `--statusline <方式>` | 安装后顺便把 Claude 的状态栏接上:`aca`、`wrap` 或 `off`(含义见[下文](#claude-的订阅额度和状态栏)) |
| `--no-setup` | 只安装程序,不打开设置,也不提示下一步 |

### 手动下载

每个[发布版本](https://github.com/cheny-00/aca/releases)里,每种机器有一个文件,旁边各有一个 `.sha256` 校验和文件:

| 机器 | 文件 |
|---|---|
| Apple 芯片的 Mac | `aca-aarch64-apple-darwin.tar.gz` |
| Intel 的 Mac | `aca-x86_64-apple-darwin.tar.gz` |
| Linux,x86_64 | `aca-x86_64-unknown-linux-gnu.tar.gz` |
| Linux,arm64 | `aca-aarch64-unknown-linux-gnu.tar.gz` |

1. 下载对应的文件和它的校验和文件(在发布页面上点击下载,或者用 curl):

   ```sh
   f=aca-aarch64-apple-darwin.tar.gz       # 换成上表里你的那个
   curl -fLO https://github.com/cheny-00/aca/releases/latest/download/$f
   curl -fLO https://github.com/cheny-00/aca/releases/latest/download/$f.sha256
   ```

2. 核对、解压。压缩包里只有一个文件 `aca`。

   ```sh
   shasum -a 256 -c $f.sha256              # Linux 上用 sha256sum -c $f.sha256
   tar -xzf $f
   ```

3. 放进 `PATH` 里的目录:

   ```sh
   mkdir -p ~/.local/bin && mv aca ~/.local/bin/
   ```

4. **只有 Mac、而且是用浏览器下载的才需要:** 去掉浏览器给它打的标记。aca 没有用苹果开发者证书签名,
   不去掉的话 macOS 不让运行浏览器下载的文件。用 curl 下载的没有这个标记。

   ```sh
   xattr -d com.apple.quarantine ~/.local/bin/aca
   ```

然后接着看[快速上手](#快速上手)。

### 从源码编译

需要:

- **Rust 1.89 或更新的版本。** 用 [rustup](https://rustup.rs) 安装;Linux 发行版自带的 Rust 往往比这个旧。
  `cargo --version` 可以看现在的版本。
- **C 编译器。** aca 把 SQLite 一起编进程序里。macOS 上运行 `xcode-select --install`(Rust 在 macOS 上本来也要它);
  Debian / Ubuntu 上 `sudo apt install build-essential`;Fedora 上 `sudo dnf install gcc`;Arch 上 `sudo pacman -S base-devel`。
- **git**,以及第一次编译时能上网(cargo 要下载依赖)。

然后:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # 还没有 Rust 时才需要;装完开一个新终端
git clone https://github.com/cheny-00/aca && cd aca
./install.sh
```

`./install.sh` 编译出发布版(第一次要几分钟),安装到 `~/.local/bin/aca`,和下载安装的效果一样:同样的参数都能用
(`--prefix`、`--statusline`、`--no-setup`),也不会安装别的东西。第一次在终端里运行时,安装完成后会打开 `aca setup`,按 Esc 可以跳过。

想编某个发布的版本而不是最新代码,先切到它的 tag(例如 `git checkout v0.1.0`)。
不想留一份源码的话,在安装脚本那条命令后面加上 `--from-source`:它会用 `cargo install` 在临时目录里编译出同样的程序。

### 升级

升级不会动你的设置和会话。

- **安装脚本:** 把同一条命令再运行一遍。
- **手动下载:** 照上面的步骤再做一遍,替换掉原来的文件。如果你运行过 `aca tmux install`,aca 的 tmux 设置会在下次打开 aca、
  或在界面里按 `Ctrl+L` 时更新到新版本(用安装脚本升级时会立即更新)。
- **从源码编译:** `cd aca && git pull && ./install.sh`

正在运行的 agent 不受影响。已经打开的界面仍是旧版本,在界面里按 `Ctrl+L` 即可原地重启为新版本。

要删掉 aca,见[卸载](#卸载)。

## 快速上手

```sh
aca setup                 # 可以跳过,说明见下
aca                       # 打开界面
```

1. 在界面里按 `n`,选择 Claude 或 Codex,确认目录,会话随即启动。
2. aca 会直接进入这个会话,像平时一样和 agent 对话即可。
3. 按 `Ctrl+Q` 回到列表,agent 继续运行。在列表里选中会话按 `回车` 可以再次进入。
4. 再新建几个会话。留意每个会话前面的状态标记;按 `i` 可以打开下一个待处理的会话。
5. 随时按 `?` 查看快捷键(常用的列在[按键](#按键)里)。

`aca setup` 是一个设置页面,可以登记额外的账号、选择要不要让 aca 显示 Claude 的订阅额度。
按回车之前不会修改任何内容,按 Esc 直接退出。如果你只用默认的 `~/.claude` 和 `~/.codex`,可以跳过这一步。

界面默认是英文。想用中文:在界面里按 `Ctrl+S` 再按 `l`,或者设置环境变量 `ACA_LANG=zh`。

**第一次用?[上手指南](docs/onboarding.zh-CN.md)带你走完最初的十五分钟**,
包括每个 agent 第一次启动时会问的两个问题,以及怎样让它们以后不再问。

## 按键

在界面里按 `?` 打开一屏的快捷键速查表:在速查表里按某个键,会高亮它的说明,但不会执行。`aca help keys` 有每个键的详细说明。

**在列表里**

| 按键 | 作用 |
|---|---|
| `↑` `↓` 或 `j` `k` | 上下移动;按数字直接选中对应编号的会话 |
| `回车` | 进入会话。已停止的(`■`)会先启动,继续原来的对话 |
| `n` / `N` | 新建 / 按当前配置新建(沿用工具、账号和目录) |
| `i` | 打开下一个待处理的会话(`◆` 等待回应或 `◐` 未读) |
| `空格` | 每一行显示一个字母,按对应的字母直接进入 |
| `/` | 过滤列表 |
| `z` | 全屏查看输出,不进入会话 |
| `x` | 查看改动:会话工作目录的 git diff |
| `Z` | 按轮次查看对话记录:执行了什么、改了什么、每一轮的花费 |
| `e` / `s` | 写便签,即准备对它说的下一句话(在分组上写的是整个分组的备注;`Esc` 保存)/ 把便签发送给它 |
| `f` | 分叉:新建一个会话,从当前对话继续 |
| `H` | 交接:新建一个会话(工具、账号可另选),接手它的工作 |
| `X` | 切换账号继续这段对话,用于当前账号额度用完时 |
| `r` / `R` / `D` | 重命名 / 重启 / 停止 |
| `d` | 删除:在分组上会连同其中的会话一起删除。删除的内容在回收站保留 1 天,`Ctrl+Z` 撤销 |
| `b` | 回收站:保留 1 天,`⏎` 恢复;`v` 多选,批量恢复或永久删除 |
| `m` / `p` | 移到分组 / 置顶 |
| `v` `V` | 多选(同 yazi):`v` 后上下移动选中一段(`Esc` 确定),`V` 同样移动但取消选中,`空格` 选中或取消一个;之后按 `d` `D` `R` `m` `u` `p` `Q` 对所有选中的会话执行,`Ctrl+Z` 一步撤销 |
| `Tab` | 分组列表:只看某个分组,或查看全部 |
| `$` | 用量和花费 |
| `P` / `:` | 插件 / 命令:列出 aca 和插件能对选中行执行的操作,每行带直达键,`/` 筛选 |
| `Ctrl+S` | 设置:主题、语言、账号和登录 |
| `Ctrl+L` | 原地重启界面,例如安装新版本之后 |
| `q` `q` | 退出 aca,会话继续运行。在 tmux 弹窗里按 `Esc` 也能关闭 |

**在会话里面**

| 按键 | 作用 |
|---|---|
| `Ctrl+Q` | 回到列表,agent 继续运行 |
| `Ctrl+Alt+N` | 直接进入下一个待处理的会话 |

这两个键只在 aca 启动的会话里有效,在其他地方会照常传给里面的程序。在其他 tmux 会话里,用 tmux 前缀键加 `d` 返回。

**在任何 tmux 会话里,运行过 `aca tmux install` 之后**(可选,见[可选的工具](#可选的工具))

| 按键 | 作用 |
|---|---|
| `Ctrl+\` | 在右侧打开 aca 作为切换器;高亮移到哪个会话,旁边就切换到哪个 |
| 前缀键,再按 `a` | 全屏打开 aca |

**改键。** 界面里的键是固定的,不能改。下面这些可以改:

- `Ctrl+Alt+N`:`aca tmux key next <键>`,键名用 tmux 的写法,比如 `M-n`、`F7`。它记在 `config.toml` 的
  `[keys] next` 里,设为 `""` 表示不绑定。
- `Ctrl+\` 和前缀键 + `a` 写在 `~/.config/aca/tmux.conf` 里,升级 aca 时这份文件会被重写。想换成别的键,
  在你自己的 `tmux.conf` 里、加载这份文件的那一行后面 unbind / bind。
- `Ctrl+Q`:如果你自己在 tmux 里绑定过它,aca 不会修改你的设置;这时用前缀键 + `d` 返回。
- 插件的键(`:` 菜单里的和会话里面的):在插件页(`P`)里改,或者用 `aca plugin hotkey`。见
  [docs/plugins.zh-CN.md](docs/plugins.zh-CN.md)。

## 常用命令

界面里能做的事都有对应的命令。会话的 id 只写开头几位就行,也可以直接写会话的标题。

```sh
aca new claude ~/code/app                 # 在某个目录里启动一个 Claude 会话
aca new codex . -t "修登录" -g work       # 指定标题,并放进一个分组
aca new claude:work . --say '先读一下 README'    # 使用名为 work 的账号,并带上第一句话
aca ls                                    # 列出所有会话
aca attach 3f2a                           # 进入会话(Ctrl+Q 回来)
aca send 3f2a -m '接着做第二步'           # 在会话的输入框里输入一句话并回车
aca wait 3f2a                             # 等待它完成这一轮、提出问题或出错
aca answer 3f2a 2                         # 替它选第 2 个选项
aca interrupt 3f2a                        # 方向不对时:中断它正在运行的这一轮
aca stop 3f2a && aca start 3f2a           # 停止,再在原处启动并继续原来的对话
aca fork 3f2a -t "试试另一个方案"         # 从它当前的对话分出一个新会话
aca rename 3f2a "登录页改版"              # tmux 会话名也一起修改
aca cost                                  # 用量和花费
aca help                                  # 一屏列出所有命令;aca help <命令> 查看某个命令的详细说明
```

输出到终端时会排版并着色;通过管道传给其他程序时是纯文本。
`ls`、`show`、`new`、`adopt` 加上 `--json` 输出供程序读取的 JSON。

## 配置

所有配置都是可选的。没有配置文件 aca 也能正常使用。

配置写在 `~/.config/aca/config.toml` 里,修改后立即生效,无需重启。文件不是合法的 TOML 时,aca 会提示一次,
然后按空配置继续运行。一般不需要手动编辑:大多数设置都能在设置页(`Ctrl+S`)里修改,并自动写入这个文件;
在设置页按 `e` 会用编辑器打开它。

下面是最常改的几项:

```toml
[tools.claude]
account = "work"                          # aca new claude 没有指定账号时使用的账号
args = ["--model", "opus"]                # 每次启动都带上的参数(也可以在账号页按 p 修改)

[tools.codex]
hook_trust = "bypass"                     # 不再询问「Hooks need review」(见上手指南)

[trust]
folder = true                             # 替你回答「是否信任这个目录」

[idle]
stop_after = "3h"                         # 界面打开时,停止空闲超过这么久的会话(⏎ 重新启动并继续原来的对话);
                                          # 空闲的 agent 也会一直占用 CPU
remove_after = "30d"                      # 删除已停止超过这么久的会话(移到回收站,1 天内可用 ^Z / b 恢复);
                                          # 写 "never" 或不写 = 不删除;置顶的、有便签的不会删除

[keys]
next = "M-n"                              # 「进入下一个待处理的会话」改用 Alt+N,而不是 Ctrl+Alt+N

[login]
browsers = ["Firefox", "Safari"]          # 登录链接可以用哪些浏览器打开(不写:列出已安装的浏览器)

[ui]
lang = "zh"                               # "en" 或 "zh"
menu = ["work", "personal"]               # n 菜单里哪些排在前面

[tools.aider]                             # 接入其他 agent:只需声明,不用写代码
resume = ["--session", "{conv}"]
busy = ['Working\.\.\.']
```

全部的配置节,以及去哪里看详细说明:

| 配置节 | 用来做什么 | 详细说明 |
|---|---|---|
| `[accounts.claude]` `[accounts.codex]` | 额外的账号 | [账号](#账号) |
| `[tools.<名字>]` | 内置的 Claude、Codex、shell 的默认设置(账号、参数、环境变量、外层包装的启动命令);自己命名的变体;不用写代码就能接入的其他 agent | `aca help config` |
| `[trust]` | 替你回答「是否信任这个目录」 | [上手指南](docs/onboarding.zh-CN.md#第一次会被问到的两个问题) |
| `[idle]` | 把空闲了一段时间的会话停掉 | `aca help stop` |
| `[inbox]` | 派出去的会话在哪些情况下通知派它的会话:完成、失败等 | `aca help msg` |
| `[keys]` | aca 绑定在 tmux 上、进入下一个待处理会话的键 | [按键](#按键) |
| `[login]` | 登录链接可以用哪些浏览器打开;通过 SSH 连接时改为在其他设备上完成登录 | `aca help config` |
| `[ui]` | 语言;`n` 菜单的顺序,以及要隐藏的项 | `aca help config` |
| `[mcps.<名字>]` | 你的 MCP server 目录:用 `M` 挂到某个项目上,或者通过 gateway 插件提供给所有 agent | [docs/plugins.zh-CN.md](docs/plugins.zh-CN.md) |
| `[theme]` `[ime]` `[new_group]` | 主题、自动切换输入法(只有 macOS)、新分组的默认目录 | 在设置页(`Ctrl+S`)里改 |

- `aca help config` 列出每一项配置并附有示例;`aca names` 列出现在能用的账号名和工具名。
- 插件的设置单独放在 `~/.config/aca/plugins.toml` 里,在插件页(`P`)里改。见 [docs/plugins.zh-CN.md](docs/plugins.zh-CN.md)。
- 环境变量在当前终端里优先于配置文件:`ACA_LANG`(`en` / `zh`)、`ACA_THEME`、`ACA_IME`。
  `ACA_HOME` 把 aca 的所有文件放到另一个目录,适合用来测试。

## 账号

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

## 不是用 aca 启动的会话

不是 aca 启动的会话(比如你手动在 tmux 里运行的 `claude`)会出现在界面的「其他 tmux 会话」里。
你可以查看、进入它们,但不能重命名、重启或分叉。要把它交给 aca 管理(**无需停止**):

```sh
aca adopt                      # 列出可以接管的会话
aca adopt my-session           # 接管其中一个(界面里按 A)
aca adopt --all
aca release <id>               # 移出 aca:只删除登记,tmux 会话和其中的进程保留
```

aca 会判断这个会话用的是哪个工具、哪个账号,以及它正在进行哪一段对话。
在第一次重启之前,接管的会话没有 aca 的 hook,所以它的状态只能靠读屏幕来判断,
也不会出现「未读」状态。运行一次 `aca restart <id>`(继续原来的对话)之后,它就和其他会话一样了。

之前用 [agent-deck](https://github.com/asheshgoplani/agent-deck)?`aca import agent-deck` 会列出它的分组和会话,
加上 `--apply` 才会实际导入。仍在 agent-deck 中运行的会话,建议用上面的接管方式。

## 可选的工具

aca 不会替你安装下面这些工具,一个都不装也能正常使用;每个工具只开启表中对应的功能。
需要哪个,用包管理器自行安装即可。

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

另外两项需要你自己开启:

```sh
aca tmux install               # 添加 tmux 快捷键,在任何会话里都能弹出 aca;
                               # 并在 tmux 状态栏显示待处理会话的数量。会在你的 tmux.conf 里加两行;
                               # aca tmux uninstall 移除这两行
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
aca plugin timer               # 列出插件的定时任务;关闭已开启的:aca plugin timer off <插件.能力>
rm ~/.local/bin/aca
rm -rf ~/.config/aca ~/.local/share/aca ~/.local/state/aca ~/.cache/aca
```

以上操作都不会关闭你的 tmux 会话和其中的 agent。由 aca 启动的会话会在 hook 里调用 `aca` 这个程序,
所以删除之后请关闭或重启那些 agent。你和 Claude、Codex 的对话保存在它们各自的目录里,不受影响。

## 文档

- [上手指南](docs/onboarding.zh-CN.md):最初的十五分钟
- `aca help`、`aca help <命令>`、`aca help keys`、`aca help config`:完整的参考,内容始终和你安装的版本一致
- [docs/plugins.zh-CN.md](docs/plugins.zh-CN.md):使用和编写插件
- [jobhook](https://github.com/cheny-00/jobhook):远程任务插件(`aca.remote`)背后的小型服务端,是一个单独的项目

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

## 致谢

aca 是参考 [agent-deck](https://github.com/asheshgoplani/agent-deck) 做的:把所有 agent 会话放在一个终端界面里、每个会话跑在自己的 tmux 会话里,
以及接入其他 agent 的方式,都来自 agent-deck。aca 是用 Rust 另外写的独立程序,不是它的分支(fork)。
感谢 agent-deck 和它的贡献者。

## 许可证

[MIT](LICENSE)
