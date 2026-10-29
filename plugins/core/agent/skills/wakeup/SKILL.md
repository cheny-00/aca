---
name: wakeup
description: Use when you start something that takes several minutes or more (a build, CI, a test suite, a deploy, a long download or migration) and would otherwise sleep or poll until it's done. aca can wake you with a message later, so you end the turn instead of waiting in it.
---

# Wake yourself later

You run in an aca session (`$ACA_SESSION_ID` is set), and aca can hand you a message after a delay, as a new turn:

```sh
aca msg --in 10m "$ACA_SESSION_ID" "what to check, where, and what to report"
```

1. Start the slow thing in a session of its own, so it outlives this turn. A plain `nohup <command> &` doesn't:
   when your command returns, its whole process group can be killed. Works on macOS and Linux:
   ```sh
   python3 -c 'import os, sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])' <command> > <log file> 2>&1 < /dev/null &
   ```
   (`setsid nohup <command> > <log file> 2>&1 < /dev/null &` where there is `setsid`, e.g. Linux.) Note the PID.
2. Schedule the wakeup. `--in` takes 90s, 10m, 1h30m (7 days at most): about when it should be done, plus a little.
   The text comes back to you word for word, so put in what you'll need then: what you started, the directory,
   the log file or PID, what to check, and what to report.
3. End the turn: say what you started and when you'll check. Don't sleep or poll in this turn.
   (A quick `ps -p <PID>` first is fine, to see it's running.)
4. When the message comes, check. Not done yet: schedule another one. Done: finish the job as asked, e.g. report the result.

Something that's done within a minute or two: just wait for it.
