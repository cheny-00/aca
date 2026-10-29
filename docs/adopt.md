# Sessions you started without aca

English | [简体中文](adopt.zh-CN.md)

How to bring sessions you started by hand under aca without stopping them, and how to import from agent-deck.

Sessions that aca did not start (for example a `claude` you launched by hand in tmux) appear in the
interface under "Other tmux sessions". You can look at them and go in, but not rename, restart or fork
them. To bring one under aca **without stopping it**:

```sh
aca adopt                      # list the sessions that can be adopted
aca adopt my-session           # adopt one (in the interface: A)
aca adopt --all
aca release <id>               # let go: unregister only, the tmux session and its processes stay
```

aca works out which tool and account the session belongs to and which conversation it is in. Until its
first restart an adopted session has no aca hooks, so its state is read from the screen only and
"finished, not read yet" is not available. `aca restart <id>` restarts it once, resuming the same
conversation, and after that it behaves like any other session.

Coming from [agent-deck](https://github.com/asheshgoplani/agent-deck)? `aca import agent-deck` lists its
groups and sessions, and `--apply` brings them over. Sessions that are still running there are better
adopted as above.
