# Accounts

English | [简体中文](accounts.zh-CN.md)

How to run the same tool under several accounts side by side, and how to see Claude's subscription limits.

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

## An account kept in a sandbox

For an account that must stay apart from this Mac, such as one an employer issues. It lives in a cage you
reach over ssh (a container, a virtual machine, another computer), and aca starts its sessions in there.
Directories of this Mac are mounted into the cage at the same path, so a path means the same on both sides.

```toml
[sandbox.work]
ssh = "work"                     # a Host in ~/.ssh/config
up = "~/cage/up"                 # run first when ssh can't connect
dirs = ["~/work/app"]            # project directories mounted into it
workspace = "~/cages/work"       # mounted too: claude/ is the cage's Claude configuration directory,
                                 # worktrees/ holds its worktrees
space = "3"                      # the workspace in the interface its sessions go to

[accounts.claude]
team = { sandbox = "work" }      # no home: its configuration directory is the cage's

[accounts.codex]
team = { sandbox = "work" }      # Codex the same
```

- `aca new claude:team ~/work/app` runs Claude in the cage over ssh. The session is listed, previewed and
  entered like any other. Only the tool's own arguments and the `env` of `[sandbox.work]` and of the account go in. Hooks and other files
  of this Mac don't, so its state comes from what Claude reports itself and from the screen.
- Its sessions start only in a directory listed in `dirs`, or in the workspace's `worktrees/`, which is
  where `-w` puts the worktrees it creates.
- `aca run claude:team` runs in the cage too, so that is where you log in. The login and conversations are
  in `<workspace>/claude/`, which this Mac can read.
- Codex works the same way, with `<workspace>/codex/` as its configuration directory. `aca run codex:team
  login` logs in there with a browser on this Mac: open the link it prints, and the page it returns to after
  you sign in is forwarded into the cage, so nothing needs to be copied back. When you're connected to this
  Mac over SSH it uses a device code instead (a ChatGPT Business or Enterprise workspace has to have device
  code logins allowed by an admin). Its conversation isn't known
  until it's restarted: then aca takes its newest conversation started in the session's directory.
  Only Claude Code and Codex accounts can be bound for now.
- `aca new shell:work` opens a terminal in the cage, with nothing to configure; in the interface it's
  listed when you press `^E`. It starts in the session's directory when that's mounted, and where a
  login to the cage lands otherwise.
- **git on a mounted directory runs in the cage.** The cage can write commands into a repository's
  `.git/config` and hooks, and git on this Mac would run them as you. So whenever aca runs git on a mounted
  directory (changes, worktrees, the status bar), it runs it in the cage over ssh. Using the directory on
  this Mac yourself, with your editor or your git, still means trusting what the cage writes there.
- **Closed.** aca sends nothing in: no session started from another session (handing work over), no
  messages from other sessions, no worker notices, no moving a conversation to or from another account, no
  `aca.remote` task run on it (under any name that reaches it). You
  can still talk to its sessions yourself. This only binds aca: an agent of yours that can read the ssh key
  can still connect.
- **Taking work in.** With `delegate = true` in `[sandbox.work]`, other sessions can hand work to it and message
  it, with `aca wait` and `--notify` as usual. What the workers do comes back to whoever handed the work over: what
  runs in the cage reaches an agent on this Mac. There are no hooks inside, so a turn's start and end come from what
  the agent reports itself. Switching accounts and `aca.remote` stay closed.

### Without a cage: another time zone, another way out, on this Mac

A `[sandbox.*]` without `ssh` runs on this Mac and only gives the accounts bound to it its `env`. It's for an
account that only has to run in another time zone, or go out through a company proxy.

```toml
[sandbox.unis]
env = { TZ = "America/Los_Angeles", HTTPS_PROXY = "http://127.0.0.1:28888", HTTP_PROXY = "http://127.0.0.1:28888", NO_PROXY = "localhost,127.0.0.1" }
workspace = "~/cages/unis"       # the config dirs of the accounts bound to it: codex/, claude/

[accounts.codex]
team-unis = { sandbox = "unis" }
```

- Its sessions start on this Mac like any account's, hooks, plugins, handing work over, messages and switching
  accounts included, only with these variables, and with `<workspace>/codex/` as the configuration directory. The
  account's own `env` wins over it. `aca run codex:team-unis login` logs in on this Mac.
- `aca new shell:unis` opens a terminal with these variables.
- **Nothing is kept apart.** Only programs that read these variables follow them. Codex's own requests to the model
  go through the proxy; commands the agent runs that ignore proxy variables (ssh, programs opening their own
  connections) go straight out, and the MCP servers Codex starts don't get the proxy variables (it passes a fixed few,
  `TZ` among them); the browser login happens in the browser on this Mac. An account that must be kept apart from this
  Mac goes in a cage, above.

In the interface both kinds are special environments: `^E` lists them apart (they're not in `n`), each environment
under a heading saying where it runs, its time zone and proxy, each account saying whether it's logged in; the list
shows them as `team@work`, `team-unis@unis`.

In `^E`, `l` on an account logs it in: a terminal opens below running the login in its environment (inside the cage
for a cage's, with unis's variables for unis's). A logged-in Codex account asks first, since Codex clears the old login
as soon as it starts. When the login link pops up, pick 「在别的电脑登录」 (log in on another computer): the link is
copied; open it on the other computer and log in, and that browser ends up on an address that doesn't load,
`http://localhost:1455/auth/callback?code=…`. Paste that address back into aca and press ⏎: aca makes the request that
browser would have made to the login waiting here. The `code` expires within minutes, so paste it back soon.

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

Per-model weekly limits (such as Fable) never reach the status bar. For an account with sessions in use,
aca asks Claude itself in the background every few minutes (the same source as `/usage`) and adds them.
Only registered accounts are asked.

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
