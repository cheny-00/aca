# Writing an aca plugin

English | [简体中文](plugins.zh-CN.md)

A plugin is a directory: an `aca-plugin.toml` manifest plus the programs it runs. Write them in any language; there
is no SDK. The manifest declares what the plugin brings. aca runs its programs at the right moments and passes what is
meant for agents on to them. When a plugin wants aca to do something, it calls the `aca` command line —
**the command line is the plugin API**.

A plugin adds things in two directions: to aca itself (keys, menu entries and panes in the interface, command-line
commands, event hooks, timers), and to agents (skills, slash commands, MCP tools).

The design follows [herdr](https://github.com/herdrdev/herdr)'s plugins. The last section lists the differences.

A plugin says what it would like; you decide (as in kitty, where a kitten doesn't bind its own keys, and you list
which commands each remote-control password may use). Which tools agents get and which keys go in the `:` menu are
set in `plugins.toml` (see [You decide](#you-decide)). For a small addition without making a plugin, write
`~/.config/aca/user/aca-plugin.toml` (see [Your own plugin](#your-own-plugin)).

## Your first plugin

```sh
aca plugin new ./hello            # creates a manifest and a script: one command, one key, one setting, an event hook commented out
aca plugin check ./hello          # checks without linking: is the manifest valid, are its programs and scripts there, are its needs installed
aca plugin link ./hello           # links it: script changes apply at once; after changing the manifest, link again
aca plugin run me.hello.hello     # runs it (in the interface: press : on the session list, then h)
aca plugin emit turn.done --plugin me.hello   # fakes an event: the hook runs in the foreground and prints its output, no real agent needed
aca plugin log me.hello           # where hook output normally goes
```

When it's ready, push it to GitHub, and others install it with `aca plugin install you/repo`.

Examples to copy from: [`plugins/tailnet/`](../plugins/tailnet) (`aca.tailnet`) is a whole feature in one sh + jq
script — a list view, commands with arguments, tools for agents, re-reading to confirm a change, both languages — and
not one line of code inside aca hooks it up. [`plugins/remote/`](../plugins/remote) (`aca.remote`) is the same, with
settings (one of them secret), a timer (on only while there is work) and messages into a session's inbox.
A small one: [`plugins/examples/git-glance/`](../plugins/examples/git-glance) (an event, a command, a pane and text on
the session's row).

## The manifest

```toml
id = "example.git-glance"        # letters, digits, . : _ -
name = "Git Glance"
version = "0.1.0"
api = 1                          # the plugin API version it's written for (see "The contract"); default 1. Newer than this aca: it won't load
min_aca_version = "0.1.0"        # optional; newer than this aca: it can't be linked
description = "After each turn, show a summary of the changes on the session's row"
needs = ["git"]                  # optional: outside programs it uses (looked up in PATH; "a|b" = either one). aca doesn't install them;
                                 # installing, aca plugin check, aca plugin ls and the plugins page say which are missing on this machine
requires = ["aca.core"]          # optional: plugins it needs first; only first-party plugins that ship with aca; installed along with it
                                 # (the keys above go before the first [table] header: in TOML, everything after a header belongs to that table)

[settings.max_lines]             # a setting: a default and a description; the value lives in plugins.toml and reaches programs as ACA_SETTING_MAX_LINES
default = 20                     # values are read as the default's type: typing "20x" in the interface or aca plugin setting says it must be an integer
desc = "At most how many lines of the change summary to show"
# secret = true                  # for webhook URLs, keys and the like: masked where the interface and aca plugin setting list them
                                 # (the value is still stored in plain text in plugins.toml, so aca writes that file as 0600, readable only by you)

[[events]]                       # an event hook: runs a program when something happens, without waiting for it
on = "turn.done"                 # also "turn.*" or "*"
if = { parent = true }           # optional: when it doesn't match, no process is started (see "if on event hooks" below)
command = ["sh", "stat.sh"]

[[commands]]                     # a command (the old [[actions]] still works)
id = "refresh"                   # letters, digits, : _ - (no dots)
title = "Refresh the change summary"
desc = "Recompute this session's change summary"   # the description agents see; defaults to title
contexts = ["session"]           # session / group / global: where it makes sense
args = { path = "path?", top = "int?" }   # arguments: string / path / int / bool; a trailing ? makes one optional
expose = ["menu", "cli"]         # who can use it: menu (the interface's menu and keys), cli (aca plugin run), agent (an agent tool),
                                 # agent? (can be an agent tool but isn't by default: use it for anything that changes the machine; you opt in).
                                 # Default: menu + cli
input = "reply"                  # optional: what of the session it gets — reply: the last reply / screen: what's on screen now /
                                 # scrollback: the screen plus scrollback (commands and views read it from stdin; panes read the file in ACA_INPUT_FILE)
cwd = "session"                  # optional: run in the session's directory (for something like lazygit); default: the plugin directory.
                                 # ./x is still looked up there
command = ["sh", "stat.sh"]

[[commands]]
id = "close"                     # what the list view's key below runs
title = "Close the PR"
args = { number = "string" }
expose = ["cli"]
command = ["sh", "close.sh"]

[[commands]]
id = "fetch"                     # what the timer below runs
title = "Fetch from the remote"
expose = ["cli"]
command = ["sh", "fetch.sh"]

[[timers]]                       # a timer: runs one of the plugin's commands every so often (see "Timers" below)
run = "fetch"                    # a command id: no required arguments, and not session-only (there's no session when a timer fires)
every = "30m"                    # written like 30s / 10m / 2h / 1d; at least 1m
# manual = true                  # normally off; the plugin switches it itself with aca plugin timer on / off (on only while there's work).
                                 # Without it: on whenever the plugin is

[[panes]]                        # a pane: a terminal program
id = "diff"
title = "Changes"
contexts = ["session"]
placement = "popup"              # popup (default) / split / window
width = "90%"                    # the popup's size: in cells or as a percentage
height = "90%"
command = ["sh", "diff.sh"]

[[views]]                        # a view: a program's output shown in aca's full-screen reader (paging, copying; handy on a phone)
id = "ci"
title = "CI status"
contexts = ["session"]
command = ["sh", "ci.sh"]

[[views]]                        # a list view: the program prints JSON (see "Output of a list view"), one row per item; each row takes the keys below
id = "prs"
title = "PRs"
rows = true
command = ["sh", "prs.sh"]

[[views.keys]]
key = "enter"                    # one character, or enter; j k q are kept for the interface
title = "Copy the URL"
copy = "{url}"                   # copies: {name} in the template becomes that field of the row

[[views.keys]]
key = "x"
title = "Close"
run = "close"                    # runs one of the plugin's commands with this row (whether it's in the menu or not)
args = { number = "{id}" }       # arguments filled from the row; any other arguments the command needs are asked for in a form first
confirm = true                   # needs a second press
# row = false                    # doesn't need a row selected (for something like "create a new one"); templates read the list's fields

[[keys]]                         # a key in the interface's : menu that runs something directly (a command, pane or view id)
key = "c"
run = "ci"

[[hotkeys]]                      # a hotkey: pressed inside a session, no prefix (also while scrolled back); opens a pane. See "Hotkeys"
key = "C-M-g"                    # a tmux key name with Ctrl or Alt (C-], M-y, C-M-g), or F1–F12
run = "diff"                     # a pane id (hotkeys only open panes)

[agent]                          # what to bring into agents
for = ["claude", "codex"]        # which agents; default: all
dir = "agent"                    # a plugin directory in Claude's format (skills, slash commands), relative to the plugin directory
# auto = "auto"                  # the name of an on/off setting: while it's on, the agent may also use the commands in dir on its own (see below)

[agent.mcp]                      # an MCP server (stdio)
command = ["./mcp-server"]       # relative to the plugin directory
via = "auto"                     # auto: registered with the gateway when it's on, otherwise attached to the agent directly; direct / gateway force one
scope = "shared"                 # in the gateway: shared, one copy for everyone / session, one per session (for servers that keep state)
```

`command` is an argument array. It doesn't go through a shell, and it runs in the plugin directory.

Fields aca doesn't know don't stop a link, but they are named in a warning (when linking, in `aca plugin check` and on
the plugins page). Usually it's a typo, or a field that ended up under another table — in TOML, fields written after a
header such as `[[keys]]` belong to that entry.

`[agent] dir`: aca passes this directory when it starts an agent, for that launch only. It isn't installed into the
agent's config directory and doesn't touch your settings. Once the plugin is disabled or removed, the agent doesn't get
it at its next start (running ones need a restart). The directory is an ordinary Claude Code plugin
(`.claude-plugin/plugin.json` plus `commands/`, `skills/` …). A command file with `disable-model-invocation: true` in
its header only enters the context when you type that slash command. The old form `[agents.claude] plugin_dir = "claude"`
still works (Claude only).

`[agent] auto`: use it when people should decide whether the agent may use these commands on its own. It names a
setting in `[settings]` whose default is `true` or `false` (toggle it with ⏎ on the plugins page, or with
`aca plugin setting <id> <name> on|off`). On: Claude gets a copy made by aca with `disable-model-invocation` removed
from the commands and skills, so their descriptions are in Claude's list and it uses them when you ask in plain words.
Off: as the files say. Without `auto`: always as the files say. It only affects Claude (Codex's skills are always in
its list), and a change applies to sessions started or restarted afterwards. The `auto` setting of `aca.delegate` works
this way.

### How it reaches the agent (mounting)

Everything is passed at launch and holds for that launch only. aca doesn't change the account's config files or write
into your repository. When no enabled plugin brings anything to a session, its command line has no extra arguments at
all.

| | Claude | Codex |
|---|---|---|
| Plugin directory (skills, commands) | `--plugin-dir <dir>`; while the `[agent] auto` setting is on, the directory is a copy under `~/.local/share/aca/claude-plugins/<name>/local/` | aca puts a copy Codex understands in `~/.local/share/aca/codex-plugins/<name>/local/` (slash commands become skills: Codex has no custom slash commands); each account's `CODEX_HOME/plugins/cache/aca` links there (created once; config.toml isn't changed); launched with `-c plugins.<name>@aca.enabled=true` |
| The plugin's MCP server | written into an `--mcp-config` file (in `~/.local/state/aca/hooks/mcp-*.json`) | `-c mcp_servers.aca-<name>={…}`, with the session id in `env` (Codex only passes allowlisted variables to MCP processes) |
| Commands exposed to agents | the MCP server `aca` (`aca plugin tools`); tool names are `mcp__aca__<plugin>__<command>` | the same server, `-c mcp_servers.aca={…}` |

- `<name>` is the plugin id with dots changed to `-` (`aca.delegate` → `aca-delegate`).
- For both agents, a plugin's MCP server runs as `aca plugin mcp <plugin id>`: aca changes to the plugin directory,
  adds the plugin's environment (settings, `ACA_SESSION_ID`) and then runs `[agent.mcp] command` in its place.
- The Codex route depends on Codex's internal plugin cache layout, which is not a public interface. If something else is
  already at `CODEX_HOME/plugins/cache/aca`, aca leaves it alone and only attaches the MCP server that time; the reason
  goes to `~/.local/state/aca/plugin-logs/mount.log`. Codex has no "only enters the context when typed" for skills:
  a skill's name and description are always in its skill list.
- After changing a plugin, enabling or disabling it, or changing `agents` in plugins.toml, running sessions need a
  restart to see it (Codex reads plugins and tools only at launch).

### Commands as agent tools

Commands with `agent` in `expose` (plus `agent?` ones you opted in to, see [You decide](#you-decide)) are wrapped as
tools by aca's own MCP server, `aca`. The argument schema comes from `args` (`string` / `path` → string, `int` →
integer, `bool` → boolean; without `?` it's required), and the description is `desc` (or `title`). When an agent calls
one, aca checks the argument types, passes the arguments to the program as `ACA_PLUGIN_ARGS_JSON` with `ACA_SESSION_ID`
set to the calling session, and returns stdout and stderr as they are; a non-zero exit code is marked as an error.
A call runs for at most 280 seconds (Codex stops waiting after 300 by default): for longer work, have the program answer
"still running" and let the agent ask again later. Only plugins that reach this session are listed (`agents` in
plugins.toml).

### `if` on event hooks

Keys are fields of the event (`kind`; nested ones as `data.x`), plus a few about the session: `parent` (the session
that dispatched it), `tool`, `account`, `group`. The hook runs only when the values are equal; an array means any one
of them; `true` / `false` means present (non-empty) or not. For example, only transient errors of dispatched workers:
`if = { kind = "transient", parent = true }`. When it doesn't match, no process is started at all.

### Hotkeys

`[[keys]]` are keys in the interface's `:` menu; `[[hotkeys]]` are pressed while you're inside a session, facing the
agent. aca binds them in tmux, the same way Ctrl+Q takes you back:

- **Only in aca's sessions.** In other tmux sessions the key still goes to the program inside.
- **No prefix, and they work while scrolled back**: they're bound in the normal key table and in both copy-mode tables.
  Pressing one opens that pane for the session you pressed it in (`ACA_SESSION_ID`), on the terminal you pressed it on;
  a pane with `input` still gets the session's content.
- **There when installed, gone when removed**: bindings aren't written into tmux.conf. They're bound when the plugin is
  installed or enabled and unbound when it's disabled or removed; after tmux restarts, they come back the next time you
  create or enter a session.
- **They don't take your keys**: if you bound that key to something else in tmux yourself, aca leaves it, and
  `aca plugin hotkey` names it. tmux's own keys for scrolling back aren't overridden either.

Write tmux key names with Ctrl or Alt: `C-]`, `M-y`, `C-M-g`, or `F1`–`F12`. A bare character isn't allowed (it would
swallow what you type). Neither are `C-q` (aca's own) or `C-c` `C-d` `C-z` and others that mean something in a
terminal, nor those with Alt added (`C-M-c`): before the binding is in place, that key reaches the agent as Esc plus
Ctrl+C. One hotkey per pane.
When choosing a key: agents' input boxes use almost every Ctrl+letter (Claude Code even uses `C-]`), and window
managers often take Alt+letter. A key that already does something in the copy-mode tables (`M-y` with vi keys) keeps
doing that while you're scrolled back.

## You decide

A plugin says in its manifest what it would like; you decide in `plugins.toml` (the command line and the plugins page
change it too, and write to that file):

```toml
[plugins."aca.tailnet"]
tools = ["list", "add"]            # which commands agents get as tools. Default: those marked agent in the manifest; agent? ones must be named here
keys = { mounts = "M", run = "" }  # keys in the : menu: command / pane / view → one character, "" = no key; the rest follow the manifest's [[keys]]
hotkeys = { mounts = "M-m" }       # hotkeys inside sessions: pane → a tmux key name, "" = none; the rest follow the manifest's [[hotkeys]]
```

- **Agent tools**: aca checks `tools` both when it lists tools and when one is actually called; anything not in it is
  invisible to agents and can't be called. Commands marked `agent?` in the manifest (opening ports, deleting things —
  anything that changes your machine) aren't given by default. After a change, running sessions need a restart (Codex
  reads tools only at launch; with the gateway on, Claude switches at once). This is the list of tools for agents,
  **not a sandbox**: an agent with a shell can still type `aca` commands.
- **Keys**: when two plugins want the same key, the one you wrote in plugins.toml wins; between two manifests, the
  plugin registered first wins. The loser no longer quietly disappears — `aca plugin ls`, `aca plugin key` and the
  plugins page name it and say where to change it. `/` belongs to the `:` menu itself (to filter) and can't be given.
- **Hotkeys**: the same rules (yours first). You can also give a hotkey to a pane whose manifest doesn't — any pane of
  any plugin.

```sh
aca plugin allow aca.tailnet             # lists what agents can get: ✓ given ○ not given (ones that need you to opt in are marked)
aca plugin allow aca.tailnet add         # give it; deny takes it back; --reset goes back to the manifest
aca plugin key                           # every key in the : menu; the ones that didn't get their key are named
aca plugin key aca.tailnet mounts M      # change it to M; --none for no key; --reset back to the manifest
aca plugin hotkey                        # every hotkey inside sessions; the ones that couldn't be bound are named
aca plugin hotkey aca.copy pick M-y      # change it to Alt+y (switched in tmux at once); --none for none; --reset back to the manifest
```

In the interface: the plugins page (`P`) shows the highlighted plugin's details on the right, and ⏎ goes in to change
them — settings (⏎ to edit, space to toggle on/off ones, `-` back to the default), agent tools (space to toggle), and
keys in the plugin menu (the one `:` opens on the session list: ⏎ and then the key you want, ⌫ for no key, `-` back to
the manifest). Esc goes back to the list.

## Your own plugin

No plugin to create, nothing to link: write `~/.config/aca/user/aca-plugin.toml`. It works as soon as the file is
there and is gone when you delete it (like a `map` line in kitty.conf). It's written like any other plugin; the id is
always `user`, and name and version are optional. Put scripts next to it (`./x.sh` is looked up in that directory).
Changes apply at once, no sync needed.

```toml
# ~/.config/aca/user/aca-plugin.toml
[[panes]]
id = "lazygit"
title = "lazygit"
cwd = "session"                  # open in the session's directory
width = "95%"
height = "95%"
command = ["lazygit"]

[[commands]]
id = "links"
title = "Links in the reply"
input = "reply"                  # the last reply comes in on stdin
command = ["sh", "-c", "grep -oE 'https?://[^ )]+' | sort -u"]

[[keys]]
key = "g"
run = "lazygit"
```

It doesn't need an entry in plugins.toml (sync neither installs nor removes it). To disable it or change its agent
tools or keys, write `[plugins.user]` as usual. `aca plugin remove user` can't remove it: delete the file.

## In the interface

- **The `:` plugin menu**: press `:` on the list. It shows the commands, panes and views of enabled plugins that are
  exposed to `menu`, filtered by `contexts` for the highlighted row (`session` on a session, `group` on a group
  heading, `global` elsewhere; no `contexts` = anywhere). ↑↓ to pick, ⏎ to run, or press the key bound in `[[keys]]`
  directly; `/` filters. It runs for the highlighted session: that's `ACA_SESSION_ID`.
- **Argument form**: a command with `args` first opens a form with one field per argument (`bool` toggles with space,
  ones with `?` can stay empty), and runs only once the types check out.
- **Output**: when a command finishes, a one-line output goes to the bottom bar and anything longer to the full-screen
  reader; views always open in the reader (`r` reruns, `c` copies, `Esc` goes back). They all run in the background
  (up to 120 seconds) and don't block anything else.
- **Panes**: open in a tmux popup, split or new window. When the interface isn't inside tmux (a phone that ssh'd
  straight in, say), it hands over the terminal and runs the pane in the foreground.
- **List views** (`rows = true`): open as a page, one row per item, with the highlighted row's details below and the
  keys that row takes at the bottom. If a row lacks a field that a key's template uses (or it's empty), that key isn't
  listed on that row, and pressing it says it can't be used there. Keys that need a second press warn first. After a key
  runs, its output is shown under the list (for example, how to add back what you just removed) and the list reloads;
  `r` reloads (unless the plugin took `r`).
- **Text on a row**: text set with `aca meta set` follows the title in the list, in grey, as `[name value]`.

### Output of a list view

```json
{
  "head": "mac.tail.ts.net\npublishing off",     // grey text above the list (can be several lines)
  "warn": "tailscale isn't logged in",           // something to notice (in the warning color)
  "empty": "nothing mounted",                    // shown when there are no rows
  "fields": { "publish_to": "on" },              // what keys with row = false read
  "rows": [
    { "id": "https:443/", "mark": "●", "tone": "ok", "text": ":443 / → localhost:3000", "right": "listening",
      "detail": ["https://mac.tail.ts.net/"], "fields": { "url": "https://mac.tail.ts.net/" } }
  ]
}
```

Everything is optional. `tone` is the mark's color: `ok` / `warn` / `bad` / `dim`. In templates, `{id}` and `{text}`
are the row's own; other names are looked up in the row's `fields`, then in the list's `fields`.
`aca plugin view <plugin.view>` prints the list one row per line (`--json` prints it as it is).

## The environment a program gets

| Variable | |
|---|---|
| `ACA_BIN` | the full path of the running aca. Always call aca through it |
| `ACA_ENV` | `1` |
| `ACA_PLUGIN_ID` / `ACA_PLUGIN_ROOT` | the plugin id, the plugin directory |
| `ACA_PLUGIN_CONFIG_DIR` | for configuration the user may edit (`~/.config/aca/plugins/<id>/`) |
| `ACA_PLUGIN_STATE_DIR` | for the plugin's own state (`~/.local/state/aca/plugins/<id>/`) |
| `ACA_PLUGIN_CONTEXT_JSON` | `{"session_id", "session_dir", "session_pane", "session_parent", "invocation_source"}` |
| `ACA_PLUGIN_SETTINGS_JSON` | all settings (the manifest's defaults, with what plugins.toml changes on top) |
| `ACA_SETTING_<NAME>` | one per setting (the name upper-cased, `-` and `:` changed to `_`). Strings as they are, anything else in TOML syntax: `true`, `5`, `["a", "b"]` |
| `ACA_SESSION_ID` / `ACA_SESSION_DIR` | the session it's for, and its directory (set to empty when there's no session, not unset) |
| `ACA_SESSION_PANE` | the agent's pane in that session (`%5`): set when the interface or the command line targets a running session, and when a hook runs inside that session's own hook; empty in tools called by agents |
| `ACA_SESSION_PARENT` | the session that dispatched it (anything `aca new` started from inside a session hangs under that session); empty for sessions you started yourself |
| `ACA_PLUGIN_ENTRYPOINT_ID` | what's running this time: the command, view or pane id (commands also get it under the old name `ACA_PLUGIN_ACTION_ID`) |
| `ACA_PLUGIN_ARGS_JSON` | commands: the arguments given (typed per `args`; `{}` when there are none) |
| `ACA_ARG_<NAME>` | each given argument again, one variable each (the name upper-cased, `-` and `:` changed to `_`; optional arguments that weren't given are unset), so shell plugins don't have to parse JSON. Strings as they are; integers and booleans as `3`, `true` |
| `ACA_PLUGIN_EVENT` / `ACA_PLUGIN_EVENT_JSON` | event hooks: the event name, the whole event line |
| `ACA_PLUGIN_TIMER` | commands run by a timer: which timer (`plugin.command`); empty otherwise |
| `ACA_INPUT_FILE` | panes with `input`: the session's content is in this file (commands and views with `input` read it from stdin). `reply` is the last reply the hook recorded; when none was recorded (a session adopted and not restarted yet) or it was cut for length, aca reads the full one from the transcript |
| `ACA_INVOKED_BY` | who called it: `menu` / `cli` / `agent` / `event` / `timer` |
| `ACA_LANG` | the interface language: `zh` / `en`. A plugin should talk in it |

aca only creates the two directories; the files in them, their format and cleaning them up belong to the plugin (to
clean up after a deleted session, subscribe to `session.removed`). Programs also inherit aca's own environment (`PATH`
and so on), with the variables above on top.

## Events

One JSON object per line. Common fields: `ts` (milliseconds), `event`, `session`.

| Event | Its own fields | When |
|---|---|---|
| `session.created` | `title` `tool` `account` `dir` `group` `parent` | created (a fork adds `fork` and `fork_of`, the session it was forked from, and its `parent` is empty: nobody dispatched it; a session taken over with `aca adopt` adds `adopted` and its current session name `tmux`) |
| `session.started` | `how`: fresh / resume / fork | the agent started |
| `session.stopped` | `idle` (when aca stopped it for idling: how many seconds it had been idle; see `[idle] stop_after` in config.toml) | `aca stop`, D in the interface; aca stopping an idle session |
| `session.renamed` | `title` `tmux` | you renamed it |
| `session.titled` | `title` `tmux` | the agent set or changed its own title |
| `session.moved` | `group` | moved to another group |
| `session.rehomed` | `tool` `account` `from` (the previous `tool:account`) | carried on under another account (`aca restart --as`, or `X` in the interface): the conversation is copied to the new account and the session restarts in place |
| `session.archived` / `session.removed` | a removal carries `title`; a session let go with `aca release` adds `released` (its tmux session is still there) | |
| `turn.started` | `prompt` | you sent a message |
| `turn.done` | `last` (the last reply) `bg` (background tasks still running) `secs` | a turn finished |
| `turn.failed` | `error` `kind` (transient = rate limit, overload, dropped stream: continuing a bit later usually works; account = quota, login, billing; fatal = retrying won't help) | a turn failed (Codex doesn't report this hook: aca fills it in from Codex's transcript when it checks the state) |
| `turn.interrupted` | `by` (when `aca interrupt` stops Claude: who did it) | you pressed Esc (Codex reports this itself), or `aca interrupt` stopped it |
| `agent.waiting` | `ask` `kind` (choice / permission / empty = can't tell) `options` (the choices of a multiple-choice question) | waiting for you to confirm or answer |
| `agent.exited` | `reason` | the agent process exited |
| `conversation.changed` | `conv` `from` | the conversation id changed (/clear) |
| `message.sent` | `id` (of this message) `from` `kind` (msg / notice) `what` (what a notice is about; null for other messages) `wake` `text` | a message was put in this session's inbox (`aca msg`, or a notice sent automatically by a worker it dispatched) |
| `message.delivered` | `id` `how` (bell / queue / type / wait) | that message reached it |
| `message.throttled` | `from` `count` `secs` | one session sent messages too often; later ones are stored without waking it |
| `ask.answered` | `by` (who answered for it; `you` = from outside) `answer` `ask` `kind` (choice / permission; a dispatch gate is `gate`) | `aca answer` answered a confirmation for it |

Every event is in `~/.local/state/aca/events.jsonl`; `aca events -f` keeps reading it.

## Timers

`[[timers]]` runs one of the plugin's commands every `every`. aca has no process that stays running, so timers rely on
macOS's launchd: each timer that's on is a LaunchAgent (`~/Library/LaunchAgents/com.aca.timer.<plugin>.<command>.plist`)
that runs `aca plugin timer tick <plugin.command>` when it's due. Runs missed while the lid was closed are made up with
a single run on wake.

- **On and off**: without `manual`, a timer is on whenever its plugin is (aca checks against the manifest when a plugin
  is linked, installed, updated, enabled, disabled or removed). With `manual = true` it's normally off and the plugin
  switches it itself: `"$ACA_BIN" plugin timer on "$ACA_PLUGIN_ID.collect"` when there's work, `off` when it's done —
  so on days with nothing to do, nothing runs. Nothing runs while the plugin is disabled; when it's enabled again, each
  timer goes back to how it was.
- **When it runs**: the same environment as running that command from the command line, plus `ACA_INVOKED_BY=timer` and
  `ACA_PLUGIN_TIMER=<plugin.command>`; there's no session (`ACA_SESSION_ID` is empty), and stdin is empty. Output goes
  to the plugin's log (`aca plugin log`). A run lasts at most 10 minutes (then its process group gets TERM, and KILL if
  it hasn't exited 3 seconds later); while the previous run is still going, the next one doesn't start.
- **Switching itself during a run** (say, `off` once the work is done): aca waits for that run to finish before
  changing launchd, so the run isn't cut off halfway by its own request.
- PATH is copied into the plist when the timer is turned on (launchd's own PATH has nothing from Homebrew in it).
- Tests and benchmarks (with `ACA_HOME` or `ACA_TMUX_SOCKET` set) don't touch launchd: the plist is written to
  `timers/` in aca's state directory and treated as installed.

```sh
aca plugin timer                       # each timer: ● on ○ off, how often, how the last run went; --json for one per line
aca plugin timer on | off <plugin.command>   # switch it (a timer without manual follows its plugin and can't be turned off)
aca plugin timer run <plugin.command>        # run it once now, as if it were due: the output is printed; runs even when off
```

`--json` prints one per line: `{"id", "every" (seconds), "manual", "on" (wanted on), "enabled" (the plugin is enabled),
"scheduled" (installed in launchd), "last" (when the last run started, 0 = never), "secs", "rc" (exit code; null when
it was stopped for running too long or couldn't start), "why"}`.

## When something fails

Hooks and timers run in the background with nobody watching. When the last run failed (a non-zero exit code, it
couldn't start, it ran too long), or a server under the gateway failed 5 times in a row and was stopped, aca names the
plugin:

- the tmux status bar segment (`aca tmux-status`) gets `⚠ bark` added (first-party plugins without the `aca.`; at most
  two names, then +N);
- the interface's top bar shows `⚠ 1 plugin errors (P)` at the right, the plugin's row on the plugins page is marked as
  failed, and the right side says where, what happened and when;
- `aca plugin ls` gives one line per failure: `⚠ me.hello: hook turn.done exit code 3 · 5m ago (aca plugin log me.hello)`.

Hooks aren't waited for: aca wraps each one in sh when it starts it and records the exit code in
`~/.local/state/aca/plugin-health/<plugin>/hook-<n>`. Only enabled plugins and the last day count. Once the same place
succeeds, it's no longer reported; disabled or removed plugins aren't reported; relinking or updating clears the
records (the manifest may have changed, so the numbering may not line up). So the exit code is how a plugin says
"this didn't work": when it failed, don't `exit 0`; when there was simply nothing to do, `exit 0`.

## Commands plugins use most

```sh
"$ACA_BIN" show "$ACA_SESSION_ID" --json     # title, state, directory, conversation id, last reply, whether anyone is attached
"$ACA_BIN" ls --json --no-screen             # all sessions
"$ACA_BIN" send <id> -m '…' --wait 600       # wait until it's idle, then type a message into its input box
"$ACA_BIN" meta set <id> <name> <value> [--ttl seconds]   # put a little text on the session's row
"$ACA_BIN" meta rm <id> <name>
"$ACA_BIN" msg <id> < message.txt            # put it in the session's inbox; delivered when it suits (a running one gets it after the turn)
"$ACA_BIN" plugin timer on | off "$ACA_PLUGIN_ID.<command>"   # switch your own timer (manual ones)
"$ACA_BIN" new / start / stop / fork / rename / move / rm
```

`meta set` is how a plugin puts things in the list: names are 1–32 letters, digits, `_` or `-`; values are cut to 80
characters; with `--ttl` it disappears on its own. `aca ls` shows it as `[name value]`; in `--json` it's under `tokens`.

## The contract (api 1)

`api = 1` in the manifest says the plugin follows this version (unset also means 1; a version newer than this aca won't
load, and linking says to upgrade aca first). Within one version aca only adds things and doesn't change them:

- **May be added**: new optional fields, new environment variables, new events, new fields in events and JSON. When
  reading JSON, skip fields you don't know; don't fail because there's one more.
- **Won't change**: the names, meaning and types of the things below. Removing or renaming one means `api = 2`.

| Stable | Described in |
|---|---|
| Manifest fields | "The manifest" |
| Environment variables passed to programs | "The environment a program gets" |
| Timers, `aca plugin timer --json` | "Timers" |
| Events and their fields | "Events" |
| The list view's JSON | "Output of a list view" |
| Agent tools: names, arguments, results | "Commands as agent tools" and below |
| The outer layer of `aca ls --json` / `aca show --json`, and `aca plugin actions --json` | below |
| Output, exit codes, time limits | below |

**Output, exit codes, time limits**

- Exit code 0 = it worked; anything else = it didn't, with no further meaning.
- stdout is the result. stderr: on the command line, in agent tools, and for commands and views in the interface, it's
  shown to the person after stdout. **A list view that succeeded only has its stdout read** (that's the JSON), and its
  stderr goes to the plugin's log (`aca plugin log`); when it fails, both are shown.
- Runs from the interface and `aca plugin view` get at most 120 seconds, agent tools 280 seconds, timers 10 minutes.
  When time is up, the process group gets TERM first, then KILL if it hasn't exited 3 seconds later. `aca plugin run`,
  event hooks and panes have no time limit.
- When the program exits but processes it left in the background still hold its output, aca takes what it has read and
  moves on without waiting for them. Anything meant to keep running in the background should send its output elsewhere.
- stdin: from the command line it's your terminal; from the interface, agent tools, hooks and timers it's empty; with
  `input`, the session's content is read from here.

**JSON that aca prints**

- Stable fields of `aca ls --json` (an array) and `aca show <id> --json` (one object): `id` `title` `state` (running /
  ask: waiting for you / unread: finished and not read yet / idle / error / exited: the agent exited / stopped: not
  running) `tool` `account` `group` `dir` `parent` (who dispatched it) `fork_of` (for a fork: the session it was forked
  from; a fork has no `parent`) `tmux` (the tmux session name) `pane` (the agent's pane) `attached` (how many terminals
  are attached) `conv` (the conversation id) `ask` (the question while it waits for you) `ran` (seconds this turn has
  run) `active` (the last time something happened) `bg` (background tasks) `locked` `created` `order` `tokens` (text set
  with `meta set`) `idle_stopped` (when aca stopped it for idling, drawn ☾ in the list; 0 otherwise).
- `run` in `aca show --json` is aca's own bookkeeping, **not part of the interface**; its fields can change at any time.
- `aca plugin actions --json`: one command per line, `{"id", "title", "contexts", "args", "expose"}`.
- Times: an event's `ts` is in milliseconds, everything else in seconds. JSON is printed compactly on one line; read it
  with `jq`, don't take it apart with sed (the layout isn't part of the interface).

**Names**

- Agent tool names are `<plugin id with dots changed to ->__<command id>`. So the ids of commands exposed to agents
  can't contain `:` or `__`, and two plugins whose ids map to the same name (`a.b` and `a-b`) can't be linked at the
  same time.
- Argument and setting names become environment variables upper-cased, with `-` and `:` changed to `_`: two that map
  to the same name can't be used together.
- Commands, panes and views share one set of ids (`[[keys]]` and the command line look them up by id alone).
- Old forms still work, but the docs only show the new ones: `[[actions]]` = `[[commands]]`, `ACA_PLUGIN_ACTION_ID` =
  a command's `ACA_PLUGIN_ENTRYPOINT_ID`, `[agents.claude] plugin_dir` = `[agent] dir`.

## First-party plugins

They live in the repository's [`plugins/`](../plugins), are compiled into aca and ship with it; none is installed by
default. `aca plugin install aca.<name>` needs no path, and a bare `[plugins."aca.core"]` (with no source) in
plugins.toml also installs on another machine with `sync`. Installing means aca writes the compiled-in files to
`~/.local/share/aca/first-party/<name>/` and registers them. That copy belongs to aca: when an aca upgrade changes it,
it's rewritten the next time an agent starts, on `sync`, or when the plugins page opens (the plugins page and
`aca plugin ls` label these "ships with aca").

| Plugin | What it gives |
|---|---|
| `aca.delegate` | `/aca:delegate` (a slash command in Claude — typing `/dele` completes to it; a skill in Codex): the lead plans, dispatches workers through aca, checks their work and reports; a worker's transient failures are continued automatically. Settings: `worker` (who to dispatch to, default `codex`), `language` (which language to answer you in; empty = yours), `say` (what to say for a worker), `max_continue`, `delay` |
| `aca.core` | aca's core abilities as agent tools (for both agents): `sessions` to look at sessions, `dispatch` to dispatch a worker (under the caller, optionally on a worktree branch), `message` to pass a message, `wait` to wait (up to 240 seconds per call), `answer` to answer for it, `interrupt` to stop it. Only workers the caller dispatched itself can be answered for or stopped |
| `aca.gateway` | the MCP gateway, see the next section |
| `aca.remote` | remote tasks, entirely in the plugin's script (sh + jq + curl + ssh): runs a long task on another machine in the background; when it ends, the result arrives as a message in the inbox of the session that sent it. In the `:` menu: `r` sends one, `t` lists them (⏎ copies the command to read its log, `x` cancels, `r` collects now), and an entry to test the server; agents get two tools, `run` and `list`. Callbacks first land on a server that's always on ([jobhook](https://github.com/cheny-00/jobhook)); settings: `url`, `token`. A timer that collects every 5 minutes is on only while something is waiting |
| `aca.tailnet` | tailnet mounts, entirely in the plugin's script (sh + jq; needs `jq`): `m` in the `:` menu is a list with one `tailscale serve` per row (and whether its target is listening); ⏎ copies the URL, `x` removes it (and says how to add it back), `a` mounts a local port, `p` turns publishing on or off (with tpub installed). Agents can list; mounting a port is `agent?`, given only when you opt in (`aca plugin allow aca.tailnet add`) |
| `aca.bark` | pushes "finished", "waiting for you to confirm" and "error" to your phone with Bark (not while someone is attached to that session). Settings: `url` (the push URL), `workers` (push for dispatched workers too), `group`; an entry in the `:` menu sends a test push |
| `aca.discord` | the same three things to a Discord channel (through a webhook; not while someone is attached to that session). Settings: `url` (the webhook URL), `mention` (the user id to @ when it's waiting for you), `workers`, `username`. Outgoing only; replying from Discord into the session isn't built yet |
| `aca.notify` | the same three things as desktop notifications on this machine (not while you're attached to that session). Settings: `command` (a different notification command, with the title and body as its two arguments; when it's empty or fails, `osascript` on macOS and `notify-send` on Linux), `workers`, `attached` (notify even while attached); an entry in the `:` menu shows a test notification |
| `aca.copy` | copies commands from a reply, entirely in the plugin's script (sh; needs `fzf`): press Ctrl+Alt+Y inside a session (a hotkey; `y` in the `:` menu) and it finds the code blocks and inline commands in the raw text of the last reply — a single one is copied right away, several pop up to pick by number; each command in a shell code block is listed on its own too. Copies to the tmux buffer, the outer terminal (OSC 52) and the local clipboard. Settings: `inline` (which inline `…` to include), `lines`, `copy` (a different copy command) |

## MCP gateway

The first-party plugin [`plugins/gateway`](../plugins/gateway) (`aca.gateway`) is a registry for MCP servers. While
it's installed and enabled:

- Agents get a single MCP server, `aca`: a thin relay (`aca plugin gateway connect`) that takes stdio in and connects
  to the gateway over a local Unix socket.
- Under the gateway: plugins' MCP servers (`[agent.mcp]` whose `via` isn't `direct`), the commands plugins expose to
  agents, and the `[mcps.<name>]` entries in config.toml that are named in the gateway's `servers` setting. Tool names
  are `<server>__<tool>` (a command's tool name is already `<plugin>__<command>`, so nothing is added).
- Each server starts the first time something needs it. With `scope` `shared` (the default), one copy runs for all
  sessions; with `session`, there's one per session, shut down when that session disconnects (use this for servers
  that keep state, such as a browser). A failed connection is retried with backoff; after 5 failures in a row, it stops.
- Who's calling: the relay reports its session when it connects, and the gateway adds `dev.aca/session` to `_meta` when
  it forwards a call. Shared plugin MCP servers read the caller from there (per-session ones also have
  `ACA_SESSION_ID` in their environment). The tools each session sees follow `agents` in plugins.toml.
- The gateway is started on demand: the first relay that finds it not running starts it (with a lock, so only one
  starts), and it exits on its own after `idle_minutes` minutes with nobody connected. Its state is in
  `~/.local/state/aca/gateway/`: one log per server (`logs/`) and one line per call (`calls.jsonl`).

```toml
[plugins."aca.gateway"]
source = "~/src/aca/plugins/gateway"
settings = { servers = ["web", "playwright"], idle_minutes = 10 }
```

```sh
aca plugin gateway status            # whether it's running, who's connected, each server's state, recent calls
aca plugin gateway restart [server]  # restart one server under it (it starts again the next time something needs it)
aca plugin gateway log <server>      # its stderr
aca plugin gateway stop
aca plugin gateway add | drop <name> # move a [mcps.<name>] from config.toml into / out of the gateway
aca plugin gateway off | on <server> [--session id]   # turn a server off / on for one session only
aca plugin gateway import [--apply]  # list the MCP servers set up in individual accounts; --apply moves them into the gateway
```

- **In the interface**: on the plugins page (`P`), press `m` on the gateway's row to go in: one row per server
  (running / not started / crashed, how many tools, calls in the last day, how many sessions use it); space moves a
  `[mcps.*]` in or out, `r` restarts, `l` shows the log, `s` stops. The top of the `M` page lists the servers this
  session can see through the gateway; space turns one off or on for this session only (Claude switches at once, Codex
  after a restart). A `[mcps.*]` moved into the gateway is no longer offered a second time under "project".
- **Bringing in MCP servers from your accounts**: `import` reads the top-level `mcpServers` in `.claude.json` in Claude
  account directories and `[mcp_servers]` in `config.toml` in Codex account directories. Entries with the same name
  and the same definition are merged into one; HTTP ones (the gateway only takes stdio for now) and ones defined
  differently in different places are marked and not moved. `--apply` writes them to `[mcps.<name>]` in aca's
  config.toml (entries already there are left alone) and moves them into the gateway. The copies in your accounts stay
  as they are: while the gateway is on, agents see both, so once you've checked that everything works, delete them
  from the accounts yourself.

With this plugin disabled, everything is attached to agents directly, as before (running sessions change after a
restart).

## plugins.toml: which plugins you want

`~/.config/aca/plugins.toml` lists the plugins you want, the way vim-plug lists them in your vimrc. Keep it with your
dotfiles, and on another machine a single `aca plugin sync` sets them all up.

```toml
[plugins."aca.delegate"]
source = "~/src/aca/plugins/delegate"   # a local directory = link; a git URL / owner/repo = clone
agents = ["claude:*", "codex:work"]    # which "tool[:account]" get what it brings; default: every agent the manifest names
settings = { max_continue = 3 }        # overrides the manifest's defaults
tools = ["dispatch"]                   # which tools agents get (see "You decide")
keys = { tasks = "T" }                 # keys in the : menu

[plugins."owner/gh-glance"]
source = "owner/gh-glance"
ref = "v1.2"                           # pinned to a tag / branch / commit
enabled = false                        # disabled for now
```

- Installing, removing, enabling, disabling and changing settings from the command line or the interface (`P`) update
  this file as they go: only that plugin's lines change; everything else, comments included, stays as it is.
- When you edit it by hand, `aca plugin sync` makes this machine match it — installs what's missing, removes what isn't
  listed, and enables or disables as written. In the interface, `e` on the `P` page opens it (at the highlighted
  plugin's section) and syncs when you come back; for changes made elsewhere (pulled in with your dotfiles), the bottom
  of the plugins page says how it differs from this machine, and `S` syncs.
- `plugins.lock` records the commit of each plugin installed from git: `sync` installs exactly those (so another machine
  ends up the same), and `update` changes it after pulling.
- Changes to settings and `agents` need no sync: the next program run or agent start uses them.
- If the file doesn't exist yet, aca writes one from the plugins currently registered the first time it's needed.

## Installing and debugging

```sh
aca plugin install aca.delegate      # one that ships with aca, no path needed; when done, it lists what the plugin brings
aca plugin install owner/repo        # from GitHub; any git URL works too. Cloned to ~/.local/share/aca/plugins/<id>
aca plugin install <git URL> --ref v1.2   # pinned to a version (update leaves it alone)
aca plugin update [id]               # pull new commits for plugins installed from git (fast-forward only); no id = all. New commits go into plugins.lock
aca plugin sync                      # make this machine match plugins.toml
aca plugin remove <id>               # stop using it: one installed from git is deleted with its directory, a linked one is left alone;
                                     # its hotkeys are unbound. Its own config and state stay (empty directories don't)
aca plugin new ./my-plugin           # create a new plugin (manifest + script); --id sets the id, default me.<directory name>
aca plugin check [dir | id]          # check the manifest (unknown fields named too), the programs and scripts it runs, needs; exit code 1 if anything's wrong
aca plugin link ./my-plugin          # link a local directory (one you're writing; after changing the manifest, link again). install with a local directory links too
aca plugin ls                        # ● enabled  ○ disabled; where it came from; which agents it brings things to; which running sessions need a restart to get them
aca plugin actions                   # commands, panes, views
aca plugin run example.git-glance.refresh top=5 --session 3f2a   # arguments as name=value
aca plugin view aca.remote.tasks     # run a view and print its output
aca plugin setting aca.delegate [worker [codex:work]]   # list the settings / read one / change one (written to plugins.toml); --reset back to the default
aca plugin allow | deny <id> [command…]   # which tools agents get (see "You decide")
aca plugin key [<id> <target> <key>]      # keys in the : menu
aca plugin pane example.git-glance diff --session 3f2a [--placement window]
aca plugin log example.git-glance    # event hook output; list views' stderr goes here too
aca plugin emit turn.failed kind=transient [--session id] [--plugin id]   # fake an event; hooks run in the foreground
aca plugin timer run aca.remote.collect   # run a timer once now, as if it were due, and print its output
aca plugin disable / enable / unlink <id>
```

Event hooks are started from the hook process and **not waited for**; their output goes to `plugin-logs/<id>.log`.
Commands run in the foreground, and their output goes straight to whoever called them. Panes: `popup` opens in a tmux
popup (or in the foreground of the current terminal when you're not in tmux), `split` opens to the right of the agent's
pane, `window` is a new window in that tmux session.

Examples are in [`plugins/examples/`](../plugins/examples): `git-glance` (an event, a command, a pane and text on the
row). The first-party plugins are ready-made examples too; `aca.tailnet` and `aca.remote` are the most complete (they
only use the plugin interface, with no code inside aca; remote also uses a timer).

aca records what plugins gave an agent when it started, in the session's runtime state (`mounts`). When you later
install, remove, enable or disable plugins or change `agents`, `aca plugin ls` and the plugins page name the running
sessions that started with something different from what they'd get now: restart them to pick it up.

[`plugins/delegate/`](../plugins/delegate) (`aca.delegate`) is ready to use. It gives Claude a `/aca:delegate` command
(typing `/delegate` completes to it) and Codex a skill called delegate: the lead plans, hands the work to workers with
`aca new -w` (setting `worker`, default `codex`; each in its own worktree), waits for them with `aca wait` (Claude in
the background, Codex in the foreground with `--any`), checks the work itself and reports. When a dispatched worker's
turn fails on a rate limit or overload, its event hook says something for it a little later (setting `say`, at most
`max_continue` times, `delay` apart). Install it with `aca plugin install aca.delegate`.

## Differences from herdr plugins

| herdr | aca |
|---|---|
| `herdr-plugin.toml` | `aca-plugin.toml`; actions, events and panes are written the same way; it adds command arguments, settings, `if` on events, and the part for agents |
| `HERDR_*` environment variables | `ACA_*`, one for one |
| a server that stays running, a socket API, `events.subscribe` | no process that stays running: events are an append-only file, read with `aca events -f` |
| `pane.report_metadata` tokens | `aca meta set` |
| pane placement: overlay / popup / split / tab / zoomed | popup / split / window (all tmux's own) |
| `[[startup]]` | none: there's no server to wait for. For something that runs every so often, write `[[timers]]` (aca sets up launchd for it) |
| `[[keybindings]]` | `[[keys]]`: bound in the interface's `:` menu (`:` then that key), so they don't fight aca's own keys; they're only defaults, which you change in plugins.toml |
| | `[[views]]`: a program's output in aca's full-screen reader, so a plugin doesn't need an interface of its own |
| `plugin install <GitHub>` | `aca plugin install owner/repo` (any git URL; `--ref` pins a version), `update`, `remove` |
| | `[agent]`: brings a plugin directory (skills, slash commands) and an MCP server to the agents aca starts |
| | `plugins.toml` + `plugins.lock` + `aca plugin sync`: managed declaratively; agent tools and keys are also yours to set here |
| | `input`: hands the session's last reply / screen to the plugin (like a kitty kitten reading a window's content) |
| | `~/.config/aca/user/`: small additions without making a plugin |
| `[[link_handlers]]`, `[[build]]` | not built yet |
