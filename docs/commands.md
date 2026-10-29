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
