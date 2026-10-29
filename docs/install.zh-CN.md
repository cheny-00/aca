# 安装

[English](install.md) | 简体中文

怎样安装、升级和卸载 aca,以及每个可选的工具带来什么功能。

aca 就是一个程序 `aca`。有三种装法,都是把它放到 `~/.local/bin/aca`(可以用 `--prefix` 换位置),不安装任何别的东西。

| 装法 | 适合 | 需要 |
|---|---|---|
| [安装脚本](#安装脚本) | x86_64 / arm64 的 macOS 或 Linux:大多数人用这个 | `curl` 或 `wget` |
| [手动下载](#手动下载) | 想每一步自己来 | `tar`、`shasum` 或 `sha256sum` |
| [从源码编译](#从源码编译) | 你的机器没有编译好的版本,或者想用最新的代码 | Rust 1.89 以上、C 编译器、git |

## 安装之前

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

## 安装脚本

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
| `--statusline <方式>` | 安装后顺便把 Claude 的状态栏接上:`aca`、`wrap` 或 `off`(含义见[账号](accounts.zh-CN.md#claude-的订阅额度和状态栏)) |
| `--no-setup` | 只安装程序,不打开设置,也不提示下一步 |

## 手动下载

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

然后接着看[快速上手](../README.zh-CN.md#快速上手)。

## 从源码编译

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

## 升级

升级不会动你的设置和会话。

- **安装脚本:** 把同一条命令再运行一遍。
- **手动下载:** 照上面的步骤再做一遍,替换掉原来的文件。如果你运行过 `aca tmux install`,aca 的 tmux 设置会在下次打开 aca、
  或在界面里按 `Ctrl+L` 时更新到新版本(用安装脚本升级时会立即更新)。
- **从源码编译:** `cd aca && git pull && ./install.sh`

正在运行的 agent 不受影响。已经打开的界面仍是旧版本,在界面里按 `Ctrl+L` 即可原地重启为新版本。

要删掉 aca,见[卸载](#卸载)。

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
