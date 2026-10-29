# Drive sessions from scripts and other agents

English | [简体中文](scripting.zh-CN.md)

Everything the interface does is also a command. A shell script, a cron job or another agent can start
sessions, give them work, wait for them, answer their questions and read their replies. This page shows
the pieces and how they fit together.

## The building blocks

A session id can be shortened to its first few characters, or replaced by the session's title.

| Command | What it does |
|---|---|
| `aca new <tool[:account]> [dir] [-t title] [-g group] [--say text]` | Start a session. Prints its id, the only line on standard output. `--say` is the first message. `-w <branch>` starts it in a git worktree |
| `aca send <id> -m <text>` | Type the text into its input box and press `Enter`. `--wait <secs>` first waits until it is idle |
| `aca wait <id>…` | Block until it finishes a turn, asks something, fails or exits. The exit code says which |
| `aca output <id>` | Its last reply |
| `aca answer <id>` | Show what it is asking: the question and the numbered options, or the command it wants to run |
| `aca answer <id> <number \| yes \| no>` | Answer it: pick that option, or allow / deny a permission prompt |
| `aca interrupt <id>` | Stop the current turn (like pressing `Esc`), when it went the wrong way |
| `aca msg <id> <text>` | Leave a message in its inbox, delivered when it is convenient (see [below](#messages-that-wait-for-a-good-moment)) |
| `aca ls`, `aca show <id>` | The list; everything about one session |

`aca send` refuses a session that is waiting for a confirmation, since `Enter` might pick the default
option. Use `aca answer` there.

### What `aca wait` returns

| Exit code | Meaning |
|---|---|
| `0` | It finished a turn |
| `2` | It is waiting for a confirmation or a choice |
| `3` | The turn failed |
| `4` | It exited or was stopped |
| `124` | `--timeout <secs>` ran out first |

Given several ids, it waits for all of them and returns the first code that isn't `0`; with `--any` it
returns as soon as one of them has something.

## A worked script

```sh
#!/bin/sh
# Hand a bug to Claude, wait for it, then ask for a test.
id=$(aca new claude ~/code/app -t fix-500 --say 'the login page returns 500, find out why')

aca wait "$id"
case $? in
  0) aca output "$id" ;;          # finished the turn: print its reply
  2) aca answer "$id"; exit 2 ;;  # it asks something: show what, and leave it to you
  *) aca show "$id"; exit 1 ;;    # it failed or exited
esac

aca send "$id" -m 'now add a test for it'
aca wait "$id" && aca output "$id"
```

While it runs, the session is in the list like any other: you can look at it with `z` or go in. When
`aca wait` stops at exit code `2`, `aca answer` prints something like:

```
3f2a9c1d is asking: Use pytest or unittest for the new tests?
  1 pytest
  2 unittest
answer: aca answer 3f2a9c1d <NUMBER>
```

## Machine-readable output

In a terminal, output is laid out and colored. Piped into another program it is plain text with full ids.
For programs:

- `--json` on `aca ls`, `aca show`, `aca new`, `aca adopt` and `aca group ls`. For example, the ids of the
  sessions waiting for you: `aca ls --json | jq -r '.[] | select(.state == "ask") | .id'`
- `aca inbox <id> --json`: its messages as a JSON array, each with its sender, text and `stage` (whether
  it was delivered).
- `aca events -f`: everything that happens to sessions (a turn starts, ends, fails, a question is asked or
  answered…) as JSON lines, as it happens. The event names and fields are in [Events](plugins.md#events).

## Messages that wait for a good moment

`aca send` types into the input box right now and needs the session idle. `aca msg` instead leaves the
message in the session's inbox, and aca delivers it when it suits the agent, without touching the input
box. Claude picks it up at once, joining the current turn if one is running. Codex queues it: an idle
Codex starts a turn for it within seconds, a busy one after the current turn.

```sh
aca msg 3f2a 'the API spec changed, see docs/api.md'
aca msg --in 1h30m 3f2a 'check whether CI passed'   # deliver it later (at most 7 days ahead)
aca inbox 3f2a                                      # what came in, delivered or not
```

A scheduled message shows as `[scheduled 12:27]` in `aca inbox` and `◷ 12:27` after the title in the list,
and the session is not stopped for being idle until it is delivered. A session can schedule one to itself,
to be woken up later.

## One agent handing work to another

An agent running in an aca session can run these same commands. A session it starts with `aca new` becomes
its child: the child hangs under it in the list, and only it (or you) can `answer` or `interrupt` the child.
From inside the child, `aca msg parent '…'` reaches the one that started it.

Two bundled plugins make this easy. Install them with `aca plugin install aca.core` and
`aca plugin install aca.delegate`, or press `P` and pick them under "Available":

- **`aca.core`** gives Claude and Codex tools that call these commands: `sessions`, `dispatch`, `message`,
  `wait` (up to 240 seconds per call), `answer`, `interrupt` and `wakeup` (a scheduled message to itself).
  `aca plugin allow aca.core` lists them.
- **`aca.delegate`** gives your agent `/aca:delegate` (a slash command in Claude, a skill in Codex): it plans
  the work, dispatches workers through aca, checks what they did and reports back. Who gets the work is
  a setting: `aca plugin setting aca.delegate worker codex:work` (default `codex`). When a worker's turn
  fails on a rate limit or overload, the plugin tells it to carry on, a few times, after a pause.

### How the parent hears that the child is done

Either it waits, with `aca wait <child>`, or it asks to be told. To be told, start the child with
`--notify`, or set it for every child in `config.toml`:

```sh
aca new codex . -t worker --notify done,failed --say '…'
```

```toml
[inbox]
notify = ["done", "failed"]    # also "exited", "waiting"; "all" for everything
```

Then the child's own hooks put a short message in the parent's inbox when it finishes a turn, fails, exits
or waits for a confirmation, and the parent is woken: who it is, what happened, its last sentence, and
`aca output <id>` for the rest. Without either, nothing is sent and nobody is woken. When the parent has
already waited for a child with `aca wait`, that child's messages are marked delivered so it is not woken
twice.

## More

- [Everyday commands](commands.md): the commands one by one
- [Configuration](configuration.md): `[inbox]` and the rest of `config.toml`
- [Plugins](plugins.md#first-party-plugins): `aca.core`, `aca.delegate` and the other bundled plugins
- `aca help`, then `aca help <command>`: `new`, `send`, `wait`, `answer`, `interrupt`, `msg`, `inbox`
