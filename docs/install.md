# Install

English | [简体中文](install.zh-CN.md)

How to install aca, upgrade it and remove it, and what each optional tool adds.

aca is a single program, `aca`. There are three ways to get it. All three put it at `~/.local/bin/aca`
(change that with `--prefix`) and install nothing else.

| Way | When | It needs |
|---|---|---|
| [Install script](#install-script) | macOS or Linux on x86_64 / arm64: most people | `curl` or `wget` |
| [Download by hand](#download-by-hand) | You want to do each step yourself | `tar`, `shasum` or `sha256sum` |
| [Build from source](#build-from-source) | No prebuilt binary for your machine, or you want the latest code | Rust 1.89+, a C compiler, git |

## Before you install

aca needs these to run, whichever way you install it. The install script checks them and tells you what
is missing, but never installs them for you.

- **macOS or Linux.** aca is developed and used every day on macOS. On Linux it builds and the basics work,
  but it has had much less testing, and plugin timers currently work on macOS only. Prebuilt binaries cover
  macOS (Apple silicon and Intel) and Linux (x86_64 and arm64) with glibc 2.35 or newer, which means
  Ubuntu 22.04, Debian 12 or later. `uname -sm` shows which machine you have.
- **[tmux](https://github.com/tmux/tmux) 3.3 or newer.** Every session aca starts is a tmux session; you do
  not need to know tmux to use aca. Check with `tmux -V`. On macOS, `brew install tmux`. On Linux, use your
  package manager, but note that Ubuntu 22.04 ships tmux 3.2a, which is too old; Ubuntu 24.04 and Debian 12
  are new enough.
- **[Claude Code](https://github.com/anthropics/claude-code), [Codex CLI](https://github.com/openai/codex),
  [pi](https://pi.dev) or [opencode](https://opencode.ai)** (any of them), installed and logged in the way you
  normally use them. Run `claude`, `codex`, `pi` or `opencode` once to make sure it answers. aca starts them, but it
  does not install them or log in for you. pi and opencode show up in the new-session menu once they are on your `PATH`.
- **`~/.local/bin` on your `PATH`** (or whichever directory you install to). If it is not, add
  `export PATH="$HOME/.local/bin:$PATH"` to `~/.zshrc` or `~/.bashrc` and open a new terminal.

Tools such as git, fzf and jq are optional: each one switches on one feature. See
[Optional tools](#optional-tools).

## Install script

```sh
curl -fsSL https://raw.githubusercontent.com/cheny-00/aca/main/install.sh | sh
```

This downloads one prebuilt file and puts it at `~/.local/bin/aca`. That is all it does:

- It installs **nothing else**: not tmux, not Rust, not fzf, yazi, lazygit or any other tool. If something
  aca needs is missing, the script tells you and leaves the installing to you.
- It does **not** edit your shell startup files, your `tmux.conf`, or your Claude / Codex configuration.
- It checks the download against its published SHA-256 checksum before installing.

Options go after `sh -s --`:

```sh
curl -fsSL https://raw.githubusercontent.com/cheny-00/aca/main/install.sh | sh -s -- --prefix /usr/local
```

| Option | What it does |
|---|---|
| `--prefix <dir>` | Install to `<dir>/bin/aca` (default `~/.local`) |
| `--version <tag>` | Install a specific release, for example `v0.1.0` (default: the latest) |
| `--from-source` | Build with cargo instead of downloading (see [Build from source](#build-from-source) for what it needs) |
| `--statusline <how>` | Also connect Claude's status bar right away: `aca`, `wrap` or `off` (see [Accounts](accounts.md#claudes-subscription-limits-and-status-bar)) |
| `--no-setup` | Only install the binary; don't open setup or print next steps |

## Download by hand

Every [release](https://github.com/cheny-00/aca/releases) has one file per machine, each with a `.sha256`
checksum file next to it:

| Machine | File |
|---|---|
| Mac with Apple silicon | `aca-aarch64-apple-darwin.tar.gz` |
| Mac with Intel | `aca-x86_64-apple-darwin.tar.gz` |
| Linux, x86_64 | `aca-x86_64-unknown-linux-gnu.tar.gz` |
| Linux, arm64 | `aca-aarch64-unknown-linux-gnu.tar.gz` |

1. Download your file and its checksum, from the releases page or with curl:

   ```sh
   f=aca-aarch64-apple-darwin.tar.gz       # yours, from the table above
   curl -fLO https://github.com/cheny-00/aca/releases/latest/download/$f
   curl -fLO https://github.com/cheny-00/aca/releases/latest/download/$f.sha256
   ```

2. Check it and unpack it. The archive holds one file, `aca`.

   ```sh
   shasum -a 256 -c $f.sha256              # on Linux: sha256sum -c $f.sha256
   tar -xzf $f
   ```

3. Put it on your `PATH`:

   ```sh
   mkdir -p ~/.local/bin && mv aca ~/.local/bin/
   ```

4. **Only on a Mac, and only if a browser downloaded it:** remove the mark the browser put on it.
   aca is not signed with an Apple developer ID, so macOS refuses to run a browser download until you do.
   Files fetched with curl do not get this mark.

   ```sh
   xattr -d com.apple.quarantine ~/.local/bin/aca
   ```

Then go on with the [Quick start](../README.md#quick-start).

## Build from source

You need:

- **Rust 1.89 or newer.** Install it with [rustup](https://rustup.rs); the Rust that Linux distributions
  ship is often older. `cargo --version` shows what you have.
- **A C compiler.** aca builds SQLite into itself. On macOS run `xcode-select --install` (Rust needs it there
  anyway); on Debian / Ubuntu `sudo apt install build-essential`; on Fedora `sudo dnf install gcc`;
  on Arch `sudo pacman -S base-devel`.
- **git**, and an internet connection for the first build (cargo downloads the dependencies).

Then:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # only without Rust; open a new terminal after
git clone https://github.com/cheny-00/aca && cd aca
./install.sh
```

`./install.sh` builds a release binary (the first build takes a few minutes) and installs it at
`~/.local/bin/aca`, just like the download: the same options work (`--prefix`, `--statusline`, `--no-setup`),
and it installs nothing else. The first time, run in a terminal, it opens `aca setup` at the end; Esc skips it.

To build a release instead of the latest code, check out its tag first (for example `git checkout v0.1.0`).
If you would rather not keep a clone, add `--from-source` to the install script's command: it builds the
same thing with `cargo install` in a temporary directory.

## Upgrade

Your settings and sessions are untouched by an upgrade.

- **Install script:** run the same command again.
- **By hand:** repeat the steps above to replace the file. If you ran `aca tmux install`, those settings catch up
  with the new version the next time you open aca or press `Ctrl+L` in it (the install script rewrites them at once).
- **From source:** `cd aca && git pull && ./install.sh`

Running agents carry on as they are. An interface that is already open keeps running the old version
until you press `Ctrl+L` in it, which restarts it in place on the new one.

To remove aca, see [Uninstall](#uninstall).

## Optional tools

None of these are installed by aca, and aca works without all of them. Each one only switches on the
feature next to it. Install the ones you want with your package manager.

| Tool | What it adds |
|---|---|
| `git` | Viewing a session's changes (`x`), starting a session in a worktree (`aca new -w`), installing plugins from a repository |
| [fzf](https://github.com/junegunn/fzf) | Fuzzy-finding a directory when creating a session (`↓` or `Ctrl+F`); the `aca.copy` plugin |
| [zoxide](https://github.com/ajeetdsouza/zoxide) | Your frequently used directories appear among the suggestions when creating a session |
| [yazi](https://github.com/sxyazi/yazi) | `y` opens a file manager in the directory the session is working in |
| [lazygit](https://github.com/jesseduffield/lazygit) | `l` on the changes page opens lazygit in that repository |
| `jq` | Needed by most bundled plugins (notifications, delegate, remote tasks) |
| A [Nerd Font](https://www.nerdfonts.com) | Icons in the status bar that aca draws for Claude |
| `$EDITOR` | `e` opens the file or the record you are looking at |

Two more things you can switch on yourself; neither is done for you:

```sh
aca tmux install               # tmux keys to pop aca up from inside any session, and a count of waiting
                               # sessions in the tmux status line. Adds two lines to your tmux.conf;
                               # aca tmux uninstall removes them
aca completion zsh             # prints a zsh completion script; aca help completion explains where to put it
```

## Where aca keeps its files

| | |
|---|---|
| Configuration | `~/.config/aca/` (`config.toml`, plugin choices and settings) |
| Sessions and plugins | `~/.local/share/aca/` (`aca.db`, installed plugins) |
| Runtime state | `~/.local/state/aca/` (session states written by hooks, the event log, inboxes, logs, interface state such as pins and notes) |
| Cache | `~/.cache/aca/` |

Apart from these, aca runs tmux and starts your agents. It changes files elsewhere only when you ask it to:
`statusLine` in a Claude account's `settings.json` (`aca setup`), two lines in your `tmux.conf`
(`aca tmux install`), a project's `.mcp.json` when you attach an MCP server to it (`M`), and on macOS a
launchd entry for a plugin timer you switch on.

## Uninstall

```sh
aca setup --statusline off     # only if you connected Claude's status bar: puts your own back
aca tmux uninstall             # only if you ran aca tmux install
aca plugin timer               # lists plugin timers; switch off any that are on: aca plugin timer off <plugin.command>
rm ~/.local/bin/aca
rm -rf ~/.config/aca ~/.local/share/aca ~/.local/state/aca ~/.cache/aca
```

Your tmux sessions and the agents in them are not closed by any of this. Sessions that aca started call
the `aca` program from their hooks, so close or restart those agents after removing it. Your Claude and
Codex conversations live in their own directories and are not affected.
