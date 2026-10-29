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
