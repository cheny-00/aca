# Configuration

English | [简体中文](configuration.zh-CN.md)

What you can set in aca's configuration file, and where each part is explained.

Everything is optional. aca works with no configuration file at all.

Settings live in `~/.config/aca/config.toml`. Changes take effect at once, with nothing to restart. If the
file is not valid TOML, aca says so and carries on as if it were empty. You rarely need to edit it by
hand: the settings page (`Ctrl+S`) changes most things and writes this file for you, and `e` on that page
opens the file in your editor.

An example with the settings people change most:

```toml
[tools.claude]
account = "work"                          # the account `aca new claude` uses when you don't name one
args = ["--model", "opus"]                # arguments added every time it starts (also p on the accounts page)

[tools.codex]
hook_trust = "bypass"                     # don't ask "Hooks need review" (see Getting started)

[trust]
folder = true                             # answer "do you trust this folder?" for you

[idle]
stop_after = "3h"                         # while the interface is open, stop sessions idle this long (they
                                          # resume their conversation with ⏎); idle agents still use CPU
remove_after = "30d"                      # delete sessions stopped this long (into the trash, ^Z / b brings
                                          # them back within a day); "never" or unset keeps them. Pinned ones
                                          # and ones with a note are never deleted

[keys]
next = "M-n"                              # Alt+N instead of Ctrl+Alt+N for "next session waiting for you"

[login]
browsers = ["Firefox", "Safari"]          # the browsers offered for a login link (default: the ones installed)

[ui]
lang = "en"                               # "en" or "zh"
menu = ["work", "personal"]               # who comes first in the menu that n opens

[tools.aider]                             # another agent, declared without writing code
resume = ["--session", "{conv}"]
busy = ['Working\.\.\.']
```

Every section, and where to read more:

| Section | What it is for | More |
|---|---|---|
| `[accounts.claude]` `[accounts.codex]` | Extra accounts | [Accounts](accounts.md) |
| `[tools.<name>]` | Defaults for the built-in Claude, Codex and shell (account, arguments, environment, a wrapper command); variants you name yourself; other agents declared without writing code | `aca help config` |
| `[trust]` | Answer "do you trust this folder?" for you | [Getting started](onboarding.md#two-questions-you-will-be-asked-the-first-time) |
| `[idle]` | Stop sessions that have been idle for a while | `aca help stop` |
| `[inbox]` | What a session that was handed work tells the session that handed it over: done, failed, and so on | `aca help msg` |
| `[keys]` | The key aca binds in tmux to go to the next session waiting for you | [Keys](keys.md) |
| `[login]` | Which browsers a login link can open in; logging in on another device when you are on SSH | `aca help config` |
| `[ui]` | Language; order of the `n` menu, and entries hidden from it | `aca help config` |
| `[mcps.<name>]` | Your catalog of MCP servers: attach one to a project with `M`, or serve it to every agent through the gateway plugin | [docs/plugins.md](plugins.md) |
| `[theme]` `[ime]` `[new_group]` | Theme, input method switching (macOS only), default directory for new groups | Set on the settings page (`Ctrl+S`) |

- `aca help config` shows every setting with an example, and `aca names` the account and tool names you can
  use right now.
- Plugin settings are kept apart, in `~/.config/aca/plugins.toml`, and changed on the plugins page (`P`). See
  [docs/plugins.md](plugins.md).
- Environment variables win over the file for one terminal: `ACA_LANG` (`en` / `zh`), `ACA_THEME`, `ACA_IME`.
  `ACA_HOME` moves all of aca's files to another directory, which is handy for trying things out.
