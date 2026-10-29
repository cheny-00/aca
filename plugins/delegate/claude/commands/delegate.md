---
description: Plan → delegate to workers through aca → verify → report. You orchestrate, the workers implement. Use when the user asks to delegate a task or hand it to a worker ("用 delegate", "派给 codex").
argument-hint: [task to implement]
disable-model-invocation: true
---

# /delegate — orchestrate workers with aca

**Language:** work and think in English. Every reply to the user, the final report (Phase 5) and the
plan docs under `doc/` are in the language `aca plugin setting aca.delegate language` prints; if it
prints nothing, use the language the user writes to you in.

## Roles
- **You = orchestrator**: break the request down, write the plan, hand it off, **verify
  independently**, report. Don't write the implementation yourself except for trivial tweaks. A
  worker that looks stuck is **not** a reason to take over — see "When a worker looks stuck".
- **The worker = implementer / tester** (`aca plugin setting aca.delegate worker` says which tool and
  account, Codex by default): lands the code and writes and runs tests against the spec you give it.
  It runs as an **aca session** under yours: the user can watch it live in aca's list
  (`aca attach <id>`), and nothing about it lives inside your context.

Everything goes through the `aca` command line. `aca help <command>` explains any of them. You are
inside an aca session (`$ACA_SESSION_ID` is set), so every session you create here hangs under
you, and only you or the user can answer or interrupt it.

## Task
$ARGUMENTS

(If empty: the task is what the user just asked you to delegate. If that isn't clear, ask what to
build — don't guess.)

Run the five phases in order; finish one before starting the next.

## 1. Plan
- Read the relevant code and docs first — especially the `CLAUDE.md` / `AGENTS.md` of the directory
  you're touching — and understand the current state and its constraints.
- Non-trivial task: write `doc/<area>/<slug>-plan.md` (from `doc/_templates/plan-template.md` if the
  repo has one). It doubles as the worker's brief. Trivial task: an inline plan is enough.
- Split the work into independently shippable tasks, each with explicit acceptance criteria.
- Every brief ends with the two sections in "What every brief ends with" below. They go **into the
  brief**, not into your notes.

## 2. Hand off

### Dispatch — then arm the wake-up, every time
```bash
SLUG=<kebab>    W=$(aca plugin setting aca.delegate worker)
aca new "$W" <repo> -w delegate/$SLUG -t "$SLUG" --say "$(cat doc/<area>/$SLUG-plan.md)" \
  -- -c model_reasoning_effort=xhigh
```
(`-- -c model_reasoning_effort=…` is for a Codex worker; leave it out for a Claude one.)
- Stdout is the worker's id and nothing else. Stderr says where its worktree is:
  `worktree <repo>/.worktrees/delegate-<slug>(分支 delegate/<slug>)`. The branch is exactly the
  name you passed.
- **The brief goes in as text (`--say`), not as a path.** A worktree starts from the committed
  HEAD, so a plan doc you just wrote and didn't commit is not in it.
- **Channel**: `$W`, the plugin's `worker` setting. Another account (`aca names` lists them) means
  another home, provider and quota — that's the user's call, never yours.
- **Model**: leave it out to use the account's default. Otherwise only the newest model or the one
  just below it (`-- -m <slug>`; read the account's `config.toml` and the catalog it points at to
  see which those are). Say which one you used, and why, in the report.
- **Effort**: `xhigh` by default; `high` only for mechanical work where throughput matters more
  than depth. Nothing lower. Say which, and why, in the report.
- **Worktree — always, for any repo work.** Two agents writing one tree means the later write
  silently wins; a worktree turns that into a visible conflict when you land. Drop `-w` only when
  the target isn't a git repo, or the task needs the gitignored environment (`node_modules`,
  `.venv`, `.env` — a fresh worktree has tracked files only); then say so in the report, because
  the diff no longer attributes itself.
- Before fanning out, check `git status`: don't delegate work on files the user is editing by hand
  — their uncommitted changes aren't in the worktree, and landing would collide with them.

Then **arm the wake-up immediately**, one per worker. If you can run a command in the background
and be woken when it ends (Claude Code):
```
Bash(run_in_background: true, command: "aca wait <id>")
```
`aca wait` returns when the worker's turn ends, and you are woken when it does. A turn it ends
while its own background tasks still run doesn't count: those wake it again. One call per
worker, so each one wakes you as it lands instead of all of them waiting on the slowest. Don't add
`--notify` to `aca new`: the wait is the one wake-up channel, and it is the one that also catches a
Codex turn that failed without a hook.

If you can't (Codex as the orchestrator): wait in the foreground, all workers at once —
`aca wait <id1> <id2> --any --timeout 1800` — handle whichever one returned, and wait again on the
rest. Exit 124 means none finished yet: check on them (below), then wait again.

### When a wait returns
The exit code says what happened, and the line it prints says the rest:

| exit | what | what you do |
|---|---|---|
| 0 | the turn ended | `aca output <id>` is its final message. Done → Phase 3. A question or a stop short of done → answer with `aca send <id> -m '…'`, re-arm the wait |
| 2 | it's waiting on a dialog | `aca answer <id>` shows the question and options. Inside the task's scope → `aca answer <id> <n | yes | no>`; otherwise ask the user. Re-arm |
| 3, transient (临时错误) | rate limit, overload, a dropped stream | This plugin tells it to carry on after 30 s, 60 s, … (up to 5 times; settings `delay`, `max_continue`). Re-arm with a delay: `sleep 90; aca wait <id>`. If it comes back failed again after that, tell the user — switching channel is their call |
| 3, account (账号问题) | quota, login, billing | Ask the user |
| 3, fatal (重试无效) | context blown, bad request, model not available | Tell the user; a fresh worker with a smaller brief, or another model |
| 4 | its process exited | `aca start <id>` resumes the same conversation. If it exits again, tell the user |

### Follow-ups
- Same task: `aca send <id> -m '<the concrete failure>'` — the command, what you expected, what
  you got. Not "this is wrong, fix it". `aca send` types into its input now and needs it idle; add
  `--wait 600` if it may still be working. Re-arm the wait after every send.
- A genuinely different task gets a new worker. One coherent task per worker.

### When a worker looks stuck
Look before you pick a lever — ten seconds, not a "give it another window":
```bash
aca show <id> --json     # state, how long this turn has run, last reply
aca ls                   # everything at a glance
```
- Running far longer than the task warrants, with nothing new in `aca output`: tell the user
  (session, channel, how long). Don't wait it out silently.
- Heading the wrong way: `aca interrupt <id>` (Esc; only while it runs), then
  `aca send <id> -m '<what to do instead>'`. Its context survives.
- **Taking over is the last rung, not a lever** — only after the above, and only once it has
  stopped (`aca interrupt` first): two writers on one tree, the later write silently wins.

### What every brief ends with
1. **A result file.** Non-trivial tasks only:

   ```markdown
   ## Before you finish
   Write your report to `doc/<area>/<slug>-result.md` — in <the report language>, exactly these four
   headings (keep them as written):
   - **改了什么** — every file you touched, one line each, with why.
   - **怎么验的** — the exact commands you ran and their real output, pasted, not paraphrased.
     Anything you did *not* run belongs here too, said plainly.
   - **偏离了计划哪里** — anything you did differently from this brief, and why. Dropping or
     simplifying part of the approach because it turned out harder than expected **counts**, and
     is the most important thing on this list.
   - **还剩什么** — unfinished, broken, or knowingly out of scope.
   ```
   The third heading is the point: under pressure a model narrows the method it was handed and
   reports success against the narrowed version. Asking for the deviations in the same breath as
   the success claim makes the narrowing visible.

2. **The yardstick is read-only.** *Do not edit the acceptance criteria in this document, and do
   not edit existing tests to make them pass — if you think a criterion or a test is wrong, write
   that under 偏离了计划哪里 and leave it alone. Writing new tests is the job.*
   And: *when the work is done, `git add` the deliverable files and commit them on this branch with
   a one-line message — never `git add -A`. Probe scripts, dumps and screenshots are not
   deliverables: write them outside the repo (`$TMPDIR`) or leave them uncommitted.*

### Fan-out
One worker per task, each with its own worktree, launched back to back, one wait armed per worker:
```bash
for N in t1 t2; do
  ID=$(aca new "$W" <repo> -w delegate/$SLUG/$N -t "$SLUG-$N" \
        --say "$(cat doc/<area>/$SLUG-$N.md)" -- -c model_reasoning_effort=xhigh)
  printf '%s\t%s\t%s\n' "$ID" "delegate/$SLUG/$N" "<repo>/.worktrees/delegate-$SLUG-$N" \
    >> ~/.local/share/aca/delegate/$SLUG.tsv
done
```
That manifest (`mkdir -p ~/.local/share/aca/delegate` first) is the fan-out's durable record: if
your context is compacted while workers still run, it's how you find the branches to land. Keep it
out of `/tmp` and your scratchpad, and leave it behind afterwards.

### Land in the main checkout, uncommitted
```bash
WT=<repo>/.worktrees/delegate-<slug>    BR=delegate/<slug>
git -C "$WT" status --short     # the worker should have committed; look first
git cherry-pick -n "$BR"        # staged in the user's checkout, never committed
```
Cherry-pick before you remove anything — until it lands, that branch is the only copy. A conflict
here means the split wasn't independent: fall those tasks back to serial. After everything has
landed, run the full build and tests on the main checkout.

## 3. Verify — yourself, never the worker's word
A worker saying it's done means it *believes* it's done. Verify inside its worktree, as soon as its
wait fires; never touch a tree while its worker is running.

1. **Read** `doc/<area>/<slug>-result.md` — and read **偏离了计划哪里** twice. Then the diff. Where
   they disagree, the diff wins. Everything under 还剩什么 is an open issue for Phase 5.
2. **The yardstick didn't move**: `git -C "$WT" diff <base>.. -- doc/<area>/<slug>-plan.md` and the
   pre-existing tests. If either moved, the run's evidence is void — send it back.
3. **The diff is only the deliverable**: no probes, dumps or screenshots; nothing outside the task's
   scope (that's a finding — ask before accepting).
4. **Run it yourself** — the build, the tests, `/verify` and `/code-review` where they apply.
   Re-run the commands 怎么验的 claims and compare with the pasted output. A green suite that never
   reaches the changed code is not evidence; a test that asserts whatever the code does is worse
   than none.
5. **On failure**: `aca send <id> -m '<command, expected, actual>'`, re-arm the wait, repeat.

Pass bar: you saw the evidence yourself — test output, diff, run result.

## 4. Tear down what you started
After Phase 3 has passed (never before — the worker's session is where its reasoning lives, and
what you send failures back into). Copy its result file into the main checkout first: it sits under
the worktree's `doc/`, which is usually gitignored, and removing a worktree deletes ignored files
without asking.
```bash
mkdir -p doc/<area> && cp "$WT/doc/<area>/<slug>-result.md" doc/<area>/
aca rm <id>
```
That removes the session, and its worktree too when it's clean; the branch always stays. If the
worktree has leftovers, aca keeps it and says where — look, and clean up only what is junk. Sweep
the landed branches once the main checkout is green: `git branch -D delegate/<slug>`. A worker in an
error state: report it, don't remove it. If the user might want to read a worker's reasoning, say
so and let them decide.

## 5. Report
- To the user, in the report language: what was done, which files changed, how it was verified (with the
  evidence), open issues and risks, the suggested next step. Which model and effort each worker
  ran, and why. Which workers you removed, which you left, and why.
- **Honestly**: failed tests are reported as failed; skipped steps are named.
- Don't commit or push on your own — ask first. Commit messages and code comments in English
  (repo convention). Reports go under `doc/`, not into web artifacts.
