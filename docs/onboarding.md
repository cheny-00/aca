# Getting started with aca

English | [简体中文](onboarding.zh-CN.md)

This page takes you from nothing installed to several agent sessions running side by side. It should take
about fifteen minutes. Each step says what to type and what you should see.

## 1. Check what you need

aca manages sessions of agents you already have. It does not install them or log you in.

| You need | Check it | If it is missing |
|---|---|---|
| macOS or Linux | | |
| tmux 3.3 or newer | `tmux -V` | `brew install tmux`, or your Linux package manager |
| Claude Code and/or Codex CLI, logged in | run `claude` or `codex` once and make sure it answers | install it and log in the usual way |

You do not need Rust, and you do not need to know tmux. aca drives tmux for you.

## 2. Install

```sh
curl -fsSL https://raw.githubusercontent.com/cheny-00/aca/main/install.sh | sh
```

This puts a single file at `~/.local/bin/aca` and installs nothing else. (No prebuilt binary for your machine,
or you would rather download it yourself? [Install](../README.md#install) in the README has the other two ways.)
When it finishes you should see:

```
Installed aca 0.1.0 (ceaf172 2026-10-19) at /Users/you/.local/bin/aca
Next:
  aca setup    optional: register extra accounts and choose whether aca shows Claude's usage limits; Esc changes nothing
  aca          open the interface (n starts a session, ? lists the keys)
```

It may also print notes: tmux is missing or too old, `~/.local/bin` is not on your `PATH`, or some optional
tools are not installed. The first two need fixing before you go on. The optional tools can wait; see
[step 8](#8-optional-extras).

If the script says `~/.local/bin` is not on your `PATH`, add this line to `~/.zshrc` (or `~/.bashrc`) and
open a new terminal:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

## 3. Start your first session

```sh
aca
```

You see an empty list. Now:

1. Press `n`. A small menu lists what you can start: a shell, Claude, Codex.
2. Pick Claude or Codex with its number.
3. Type the directory of a project (Tab completes paths) and press `Enter`.

The session starts and aca takes you straight into it. You are now talking to the agent exactly as if you
had started it yourself. Later, `Enter` on a session in the list goes into it again.

### Two questions you will be asked the first time

These come from the agents, not from aca. You only need to deal with each once.

**"Do you trust the files in this folder?"** Claude and Codex both ask this the first time they run in a
directory. Answer it in the session. If you would rather have aca answer yes for you every time, press
`Ctrl+S` then `f` in the interface, or put this in `~/.config/aca/config.toml`:

```toml
[trust]
folder = true
```

**"Hooks need review" (Codex only).** aca learns what a session is doing (working, waiting, finished)
from small notifications called hooks, which it hands to the agent at startup. Codex asks you to approve
hooks it has not seen before. Choose **Trust all**. Codex remembers this for that account and does not ask
again. If you skip this, Codex sessions still run, but aca has to guess their state from what is on the
screen and cannot tell you when one has finished.

If you don't want to be asked at all:

```toml
[tools.codex]
hook_trust = "bypass"
```

Be aware that this lets every hook that Codex has not reviewed run in that session, not only aca's.

## 4. Come back to the list

Press **`Ctrl+Q`**. You are back in aca and the agent keeps working.

This is the one key to remember. It works in any session aca started. (In a tmux session that aca did not
start, use tmux's own detach: the prefix key, then `d`.)

## 5. Read the list

Start two or three more sessions with `n`, give each something to do, and come back with `Ctrl+Q`. The
mark in front of each session tells you its state:

| Mark | Meaning | What to do |
|---|---|---|
| `●` | The agent is working | Nothing. Let it run |
| `◆` | Needs you: a question or a permission prompt | Go in and answer. `i` takes you to the next one to look at |
| `◐` | Unread: it finished and you haven't read the result | Go in and look |
| `○` | Idle, and you have seen its last reply | |
| `✕` | Something went wrong | Go in and look |
| `■` | Not running | `Enter` starts it again and resumes the conversation |
| `☾` | Stopped by aca after sitting idle (set by `[idle]` in `config.toml`) | `Enter` starts it again and resumes the conversation |

Sessions can be put in groups, for example one group per project. A new session goes into the group of the
session you had selected; `m` moves it.

## 6. The keys worth learning first

Press `?` in the interface for the full list. These cover most days:

| Key | What it does |
|---|---|
| `↑` `↓` or `j` `k` | Move in the list |
| `Enter` | Go into the session (starts it first if it is not running) |
| `Ctrl+Q` | Come back from a session |
| `n` / `N` | New / new with the current settings (same tool, account and directory as the selected session) |
| `i` | Jump to the next session that needs you |
| `z` | Read the session's output full screen without going in |
| `x` | See what it changed (the git diff of its working directory) |
| `Z` | The conversation record, turn by turn: what it ran, what it changed, what it cost |
| `r` / `R` / `D` | Rename / restart / stop |
| `f` | Fork: a new session that continues from this conversation |
| `d` | Delete the session (`Ctrl+Z` undoes) |
| `/` | Filter the list |
| `$` | Usage and cost |
| `Ctrl+S` | Settings: theme, language, accounts |
| `q` `q` | Quit aca. Sessions keep running |

The README's [Keys](../README.md#keys) section lists a few more, the keys that work inside a session, and
which keys you can change.

## 7. More than one account

Skip this if you use one Claude login and one Codex login.

If you have, say, a personal and a work Claude subscription, each lives in its own configuration
directory. Register each one under a name:

```sh
aca account add claude work ~/.claude-work
```

A directory that is new needs one login, done with the tool itself:

```sh
aca run claude:work            # starts Claude with that account so you can log in
aca run codex:work -- login    # the same for Codex
```

From then on the `n` menu lists your accounts, and on the command line you write `claude:work`:

```sh
aca new claude:work ~/code/app
```

`aca setup` shows the same thing as one screen (press `+` to add an account), and `Ctrl+S` then `a` in the
interface opens the accounts page, which also shows which accounts are logged in. More detail is in the
[Accounts section of the README](../README.md#accounts).

## 8. Optional extras

Everything here is optional. Take what is useful.

**See Claude's subscription limits.** Run `aca setup`, move to a Claude account, use `←` `→` to choose how
to connect its status bar, and press `Enter`. `aca setup --statusline off` undoes it. The
[README](../README.md#claudes-subscription-limits-and-status-bar) explains what the choices mean and
exactly what gets changed.

**Open aca from inside any session.** `aca tmux install` adds tmux keys that pop aca up over the session
you are in (prefix then `a`, or `Ctrl+\`), and shows how many sessions are waiting in the tmux status
line. It adds two lines to your `tmux.conf`; `aca tmux uninstall` removes them. Without installing
anything, `Ctrl+Alt+N` inside a session already takes you to the next session that is waiting for you.

**Dark theme.** The default theme is light. `aca theme use mocha` switches to a dark one; `aca theme` lists
them all.

**Chinese interface.** `Ctrl+S` then `l`, or set `ACA_LANG=zh`.

**Tab completion for zsh.** `aca help completion` shows the two ways to install it.

**Extra tools.** aca can open [yazi](https://github.com/sxyazi/yazi) (`y`) and
[lazygit](https://github.com/jesseduffield/lazygit) (`l` on the changes page), and uses
[fzf](https://github.com/junegunn/fzf) and [zoxide](https://github.com/ajeetdsouza/zoxide) to help you pick
directories. aca never installs these for you. Install the ones you want yourself; the full list with what
each one adds is in the [README](../README.md#optional-tools).

**Plugins.** Press `P` in the interface to see what is available. For example `aca.notify` sends a desktop
notification when a session finishes or needs you. See [plugins.md](plugins.md).

## 9. Sessions you already have running

If you already have Claude or Codex running in tmux sessions you started by hand, you don't have to stop
them. They show up in the list in their own section, "Other tmux sessions". Select one and press `A`, or:

```sh
aca adopt                      # list what can be adopted
aca adopt <tmux session name>
```

## 10. Driving sessions from the command line

Everything above can be done without the interface, which is how scripts and agents themselves use aca:

```sh
id=$(aca new claude ~/code/app -t "fix login" --say 'the login page returns 500, find out why')
aca wait "$id"                 # returns when it finishes the turn, asks something, or fails
aca output "$id"               # its last reply
aca send "$id" -m 'now add a test for it'
```

`aca help` lists every command.

## If something is not right

| What you see | Likely cause and fix |
|---|---|
| `aca: command not found` | `~/.local/bin` is not on your `PATH`. See step 2 |
| A new session shows the agent's own welcome or sign-in screen | It is not logged in. Log in right there in the session. `aca account status` (or `Ctrl+S` then `a`) shows which ones are logged in, the defaults `~/.claude` and `~/.codex` included |
| A new session shows `■` straight away | The agent exited as soon as it started. Run `claude` (or `codex`) on its own in that directory and fix what it reports |
| A Codex session finishes but is never marked `◐` (finished, not read yet) | Its hooks were not approved. Restart it (`R`) and choose "Trust all" when Codex asks. See step 3 |
| `Ctrl+Q` does nothing | You are in a tmux session aca did not start: use the tmux prefix then `d`. Or your terminal or tmux configuration already uses `Ctrl+Q` |
| Boxes or question marks instead of icons in Claude's status bar | The status bar drawn by aca uses Nerd Font icons. Install a [Nerd Font](https://www.nerdfonts.com), or choose `wrap` in `aca setup` to keep your own status bar |
| Colors are hard to read | Try another theme: `aca theme use mocha` (dark) or `aca theme use latte` (light) |
| The interface is in the wrong language | `Ctrl+S` then `l` |

Still stuck? `aca help <command>` and `aca help keys` describe everything in detail, and you can open an
issue on GitHub.

## Upgrading and removing

Run the install command again to upgrade. To remove aca completely, follow
[Uninstall in the README](../README.md#uninstall).
