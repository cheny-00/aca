# aca

**One screen for all your AI coding agent sessions** — Claude Code, Codex, and any other agent you plug in.

English | [简体中文](README.zh-CN.md)

<!-- TODO: a screenshot or short GIF of the interface goes here -->

When you run several agents at once, it gets hard to tell which one is still working, which one stopped to
ask you something, and which one finished while you were looking elsewhere. aca puts every session in one
list, shows a live view of the one you select, and takes you into it when you press Enter.

Each session is an ordinary tmux session. If you close aca, the agents keep running, and you can still
reach them with plain tmux, including over SSH from another machine or your phone.

aca is modeled on [agent-deck](https://github.com/asheshgoplani/agent-deck); see [Acknowledgements](#acknowledgements).

## What you get

- **One list with clear states.** `●` working · `◐` waiting for you, or finished and not read yet ·
  `○` idle · `✕` error · `■` not running · `☾` stopped by aca after sitting idle (see `[idle]` below).
- **Several accounts side by side.** Personal and work subscriptions, an API key, Bedrock: sessions from
  all of them sit in the same list, and aca makes sure one account's login never leaks into another.
- **Your agent setup stays yours.** aca does not write hooks or anything else into `~/.claude` or
  `~/.codex`. It passes what it needs on the command line when it starts a session. Logging in is always
  done with the tool's own login command.
- **Stop and resume in place.** Stop a session and start it later: it resumes the same conversation in
  the same tmux pane. Other windows you opened next to it are left alone.
- **Read without going in.** Look at a session's output, its git diff, or a turn-by-turn record of the
  conversation (what it ran, which files it changed, what the turn cost) straight from the list.
- **Everything is also a command.** `aca new`, `aca send`, `aca wait`, `aca answer` and the rest do what
  the interface does, so scripts can drive sessions, and one agent can start another, hand it a task and
  be told when it is done.
- **Take over sessions that are already running.** A Claude or Codex you started by hand in tmux can be
  brought into the list without stopping it.
- **Usage and cost.** What your local Claude and Codex usage would cost at API prices, plus how much of
  your subscription limits is used.
- **Plugins.** A plugin is a directory with a manifest. It can add commands, react to events, open panes,
  and give agents skills and MCP servers. A few ship with aca: desktop notifications, delegating work to
  another session, running long jobs on another machine, and more.
- **No background service.** State is a handful of small files written by the agents' hooks. Nothing runs
  when you are not using it.

## Requirements

- macOS or Linux. aca is developed and used every day on macOS. On Linux it builds and the basics work,
  but it has had much less testing, and plugin timers currently work on macOS only.
- [tmux](https://github.com/tmux/tmux) 3.3 or newer
- [Claude Code](https://github.com/anthropics/claude-code) and/or [Codex CLI](https://github.com/openai/codex),
  installed and logged in the way you normally use them

## Install

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
| `--from-source` | Build with cargo instead of downloading (needs Rust 1.89+) |
| `--statusline <how>` | Also connect Claude's status bar right away: `aca`, `wrap` or `off` (see [below](#claudes-subscription-limits-and-status-bar)) |
| `--no-setup` | Only install the binary and print nothing about next steps |

If `~/.local/bin` is not on your `PATH`, the script says so; add it in your shell's startup file.

Prebuilt binaries exist for macOS (Apple silicon and Intel) and Linux (x86_64 and arm64, glibc 2.35 or
newer). Anywhere else, or to run the latest code, [build from source](#build-from-source).

**Upgrade:** run the same install command again. Your settings and sessions are untouched.

### Build from source

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

To upgrade, pull and run it again:

```sh
cd aca && git pull && ./install.sh
```

To build a release instead of the latest code, check out its tag first (for example `git checkout v0.1.0`).
If you would rather not keep a clone, add `--from-source` to the install command at the top: it builds the
same thing with `cargo install` in a temporary directory.

## Quick start

```sh
aca setup                 # optional, see below
aca                       # open the interface
```

1. In the interface, press `n`, pick Claude or Codex, and confirm the directory. A session starts.
2. Press `Enter` to go into it and talk to the agent as usual.
3. Press `Ctrl+Q` to come back to the list. The agent keeps working.
4. Start a few more. Watch the marks in the list; `i` jumps to the next session that needs you.
5. Press `?` any time for the keys.

`aca setup` is one screen where you can register extra accounts and choose whether aca shows Claude's
subscription limits. Nothing is changed until you press Enter, and Esc leaves everything as it was. If you
only use the default `~/.claude` and `~/.codex`, you can skip it.

The interface is in English by default. For Chinese, press `Ctrl+S` then `l` in the interface, or set
`ACA_LANG=zh`.

**New here? [Getting started](docs/onboarding.md) walks through the first fifteen minutes**, including the
two prompts each agent shows the first time and how to make them go away.

## Everyday commands

Everything the interface does has a command. A session id can be shortened to its first few characters,
or you can use the session's title instead.

```sh
aca new claude ~/code/app                 # start a Claude session in a directory
aca new codex . -t "fix login" -g work    # give it a title and put it in a group
aca new claude:work . --say 'read the README first'    # use the account named "work", with an opening message
aca ls                                    # the list
aca attach 3f2a                           # go in (Ctrl+Q comes back)
aca send 3f2a -m 'go on with step two'    # type a message into a session and press Enter
aca wait 3f2a                             # block until it finishes a turn, asks something, or fails
aca answer 3f2a 2                         # answer its question with option 2
aca interrupt 3f2a                        # it went the wrong way: stop the current turn
aca stop 3f2a && aca start 3f2a           # stop, then resume the same conversation in place
aca fork 3f2a -t "try another approach"   # branch a new session off its conversation
aca rename 3f2a "login page rework"       # the tmux session is renamed too
aca cost                                  # usage and cost
aca help                                  # every command on one screen; aca help <command> for details
```

In a terminal the output is laid out and colored; piped into another program it is plain text. Add
`--json` to `ls`, `show`, `new` and `adopt` for machine-readable output.

## Configuration

Everything is optional. aca works with no configuration file at all.

`~/.config/aca/config.toml`:

```toml
[tools.claude]
account = "work"                          # the account `aca new claude` uses when you don't name one
args = ["--model", "opus"]                # arguments added every time it starts

[tools.codex]
hook_trust = "bypass"                     # don't ask "Hooks need review" (see Getting started)

[trust]
folder = true                             # answer "do you trust this folder?" for you

[idle]
stop_after = "3h"                         # while the interface is open, stop sessions idle this long (they
                                          # resume their conversation with ⏎); idle agents still use CPU

[ui]
lang = "en"                               # "en" or "zh"

[tools.aider]                             # another agent, declared without writing code
resume = ["--session", "{conv}"]
busy = ['Working\.\.\.']
```

`aca help config` lists every setting. Most interface settings (theme, language, and so on) are changed on
the settings page (`Ctrl+S`) and saved to this file for you.

Claude, Codex and a plain shell are built in. Another agent can be declared right in this file, without
writing code, as `[tools.aider]` above shows.

## Accounts

An account is simply **the extra things you would type to start the tool as that account**: a different
configuration directory, some environment variables, a few arguments. You write them down once and aca
adds them every time.

If you only use the default `~/.claude` and `~/.codex`, you don't need to register anything.

```toml
[accounts.claude]
personal = "~/.claude-personal"           # just a different configuration directory
work = "~/.claude-work"
bedrock = { env = { CLAUDE_CODE_USE_BEDROCK = "1", AWS_PROFILE = "work" } }   # same directory, different sign-in

[accounts.codex]
work = "~/.codex-work"
team = { home = "~/.codex-team", args = ["--profile", "team"] }              # a directory plus a Codex profile
api = { env_file = "~/.secrets/openai.env" }                                  # key kept in a file, read at each start
```

| Key | Meaning |
|---|---|
| `home` | The configuration directory, passed in the variable the tool reads (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`) |
| `env` | Environment variables. A leading `~/` in a value is expanded |
| `env_file` | A file with one `KEY=value` per line. aca does not store its contents; it reads the file at each start |
| `args` | Arguments added whenever a session starts |

For the common case (a different configuration directory) you don't have to edit the file. Any of these
writes the same line:

```sh
aca account add claude work ~/.claude-work   # leave the directory out and it is ~/.claude-work
aca setup                                    # press + on that screen
                                             # or in the interface: Ctrl+S, then a, then +
```

Then use it as `claude:work`:

```sh
aca new claude:work ~/code/app
aca run claude:work                       # a new directory needs one login: this starts Claude so you can log in
aca run codex:work -- login               # the same for Codex
aca account status                        # which accounts are logged in (asked from the tools; aca never reads credentials)
```

Good to know:

- **No account named:** `aca new codex` adds nothing, and the tool uses its own default directory and
  login. Set `[tools.codex] account = "…"` to pick a default.
- **Not registered is fine too:** write the directory after the colon, as in `aca new codex:~/.codex-try`.
- **Accounts don't leak into each other.** When a session is started from inside another agent's session,
  aca removes the identity variables the outer session left in the environment (for example
  `CLAUDE_SECURESTORAGE_CONFIG_DIR`, which would make the new Claude use the outer account's login, and
  `ANTHROPIC_PROFILE`). Variables you put in the account's own `env` are kept.
- **The tool's own commands** (login, MCP management, and so on) run through `aca run`:
  `aca run claude:work -- mcp list`. It applies the account's `home`, `env` and `env_file`, but not `args`,
  which are only for starting sessions.
- **Your own `--settings` file is respected.** Claude only reads the last `--settings` it is given, so if
  you pass one, aca merges its hooks into a copy of yours and passes that. Your file is not modified.
- **Removing an account** (`aca account rm claude work`) only removes the line from `config.toml`. The
  configuration directory and its login are left alone.
- **Logging in again replaces the current login.** Tools discard the existing login as soon as their login
  command starts (`codex login` deletes `auth.json` immediately). The accounts page asks before doing this
  to an account that is already logged in.
- aca never stores or copies credentials, and never writes into these configuration directories, with one
  exception that you opt into: the status bar setting described next.

## Claude's subscription limits and status bar

Claude Code only reports your subscription limits (how much of the 5-hour and 7-day allowance is used) to
its status bar command. So for aca to show those limits, Claude's status bar has to pass through aca.
This is optional: without it everything else works, and the cost page simply has no Claude limits at the
top.

```sh
aca setup                      # one screen: a row per account, ←→ picks how to connect it, Enter applies
aca setup --statusline aca     # no screen: aca draws the status bar for every Claude account
aca setup --statusline wrap    # no screen: keep your own status bar, aca only records the limits
aca setup --statusline off     # undo: restore what you had before
```

- **aca** draws a two-line status bar: context usage, model, git branch and changes, directory; then the
  account's limits, cost and duration. (It uses Nerd Font icons.)
- **wrap** leaves your status bar as it is and puts `aca limits --` in front of your command.

This changes one thing: the `statusLine` entry in that account's `settings.json`. Nothing else in the file
is touched, your original entry is remembered, and `off` puts it back. Running Claude sessions pick the
change up right away.

If you would rather write it yourself:

```json
"statusLine": { "type": "command", "command": "~/.local/bin/aca statusline", "refreshInterval": 10 }
```

<details>
<summary>Experimental: limits without the status bar</summary>

Claude Code can load a small piece of code at startup. With this switched on, Claude sessions started by
aca report their limits without going through the status bar. It is off by default. Turn it on with
`Ctrl+S` then `m` in the interface, or:

```toml
[tools.claude]
mod = true
```

Only sessions started or restarted afterwards are affected. Nothing is written to your configuration
directory. The Claude Code interface this relies on is still marked early access, so aca checks it once
for each Claude version and quietly falls back to the status bar route if the check fails.

</details>

## Sessions you started without aca

Sessions that aca did not start (for example a `claude` you launched by hand in tmux) appear in the
interface under "Other tmux sessions". You can look at them and go in, but not rename, restart or fork
them. To bring one under aca **without stopping it**:

```sh
aca adopt                      # list the sessions that can be adopted
aca adopt my-session           # adopt one (in the interface: A)
aca adopt --all
aca release <id>               # the reverse: forget it, leave the process running
```

aca works out which tool and account the session belongs to and which conversation it is in. Until its
first restart an adopted session has no aca hooks, so its state is read from the screen only and
"finished, not read yet" is not available. `aca restart <id>` restarts it once, resuming the same
conversation, and after that it behaves like any other session.

Coming from [agent-deck](https://github.com/asheshgoplani/agent-deck)? `aca import agent-deck` lists its
groups and sessions, and `--apply` brings them over. Sessions that are still running there are better
adopted as above.

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

## Documentation

- [Getting started](docs/onboarding.md): the first fifteen minutes
- `aca help`, `aca help <command>`, `aca help keys`, `aca help config`: the complete reference, always
  matching the version you have installed
- [docs/plugins.md](docs/plugins.md): using and writing plugins
- [jobhook](https://github.com/cheny-00/jobhook): the small server behind the remote tasks plugin (`aca.remote`),
  a separate project

## Development

```sh
cargo test                               # unit tests and end-to-end tests (needs tmux; they start an isolated server)
cargo clippy --all-targets && cargo fmt
./install.sh                             # build this checkout and install it
```

Three crates: `crates/aca-core` (the session backend, a library), `crates/aca-tui` (the interface, depends
only on core) and `crates/aca` (the command line, and the entry point of the interface). Bundled plugins
are in `plugins/` and are compiled into the binary. Each command's usage and help text lives in one table
in `crates/aca/src/help.rs`; the interface keys are described in `crates/aca-tui/src/keys.en.txt` and
`keys.txt`.

**Keep manual tests isolated.** Set `ACA_HOME=<a temporary directory>` to keep configuration, the session
database and state out of your real ones, and `ACA_TMUX_SOCKET=<a name>` to use a separate tmux server.
`aca ui --snapshot 100x30 [keys]` renders one frame of the interface as text without doing anything. If
you start test agents by hand from inside an agent session, clear the outer session's variables first
(everything starting with `CLAUDE`): a tmux server passes the environment it was started with to every
session, and `CLAUDE_SECURESTORAGE_CONFIG_DIR` in particular makes an inner Claude use the outer
account's login. aca strips these itself when it starts an agent; hand-written test scripts do not.

To publish a release, push a tag such as `v0.1.0`. The workflow in `.github/workflows/release.yml` builds
the binaries that the install script downloads.

## Acknowledgements

aca was built with [agent-deck](https://github.com/asheshgoplani/agent-deck) as its reference. The idea of one terminal screen for all your
agent sessions, each running in its own tmux session, and the way other agents are plugged in, come from
agent-deck. aca is a separate program written in Rust, not a fork of it. Thanks to agent-deck and its
contributors.

## License

[MIT](LICENSE)
