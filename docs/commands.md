# Everyday commands

English | [简体中文](commands.zh-CN.md)

The commands you will use most, one example each.

Everything the interface does has a command. A session id can be shortened to its first few characters,
or you can use the session's title instead.

```sh
aca new claude ~/code/app                 # start a Claude session in a directory
aca new codex . -t "fix login" -g work    # give it a title and put it in a group
aca new claude:work . --say 'read the README first'    # use the account named "work", with an opening message
aca ls                                    # the list
aca ls -g work                            # sessions in this exact group, with their open ticket numbers
aca attach 3f2a                           # go in (Ctrl+Q comes back)
aca send 3f2a -m 'go on with step two'    # type a message into a session and press Enter
aca wait 3f2a                             # block until it finishes a turn, asks something, or fails
aca answer 3f2a 2                         # answer its question with option 2
aca interrupt 3f2a                        # it went the wrong way: stop the current turn
aca stop 3f2a && aca start 3f2a           # stop, then start in place
aca fork 3f2a -t "try another approach"   # branch a new session off its conversation
aca rename 3f2a "login page rework"       # the tmux session is renamed too
aca cost                                  # usage and cost
aca help                                  # every command on one screen; aca help <command> for details
```

A Claude router starts a new conversation with a handoff on both `aca start <id>` and `aca restart <id>`.
`aca msg <id> '…'` wakes a stopped Claude router. `aca stop --idle 3h` uses the router's own
`[router] idle` setting for it. A Codex router stays up when idle, holds messages while stopped and resumes on start.

Watch a ticket when you want its dispatcher notified. New tickets start unwatched. On an existing ticket,
omitting both switches keeps its setting. For example, for ticket 9 and a session named `worker`:

```sh
aca task add -g work -t "fix login" --watch
aca task give 9 worker --no-watch
aca task set 9 --watch
aca new claude --task 9 --no-watch       # hand it to a new session with watching off
```

In a terminal the output is laid out and colored; piped into another program it is plain text. Add
`--json` to `ls`, `show`, `new` and `adopt` for machine-readable output.
