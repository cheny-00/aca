---
description: Carry one change from proposal to archive in openspec/changes/<slug>/ — propose, apply each task through /aca:delegate, verify live, archive. Done means verified where it runs, not every box ticked. Use when the user asks to start, continue or check a spec change ("用 spec", "推进这个变更", "spec 到哪了").
argument-hint: "[idea | slug] [propose | apply | verify | archive | status] [Tn]"
disable-model-invocation: true
---

# /spec — one change, proposal to archive, on top of /delegate

**Language:** work and think in English. Replies to the user and the files under `doc/` are in the
language `aca plugin setting aca.delegate language` prints (empty: the language the user writes in).
The change's own files follow the repo's `openspec/config.yaml` (`context`, `rules`) when it says.

## What this adds to /delegate
/delegate runs one task: plan, hand off, verify, report — and is over when the session is. /spec
keeps what has to outlive a session:
- **the record**: `openspec/changes/<slug>/` — proposal, design, tasks. The ticks in `tasks.md` say
  how far it got; a new session, or one compacted mid-way, picks up from them.
- **a stop after the design**, for the user to review before any code is written.
- **"done" means verified where it runs**: checks green, deployed, the new build actually rolled, the
  acceptance exercised live. All boxes ticked is not done.
- **the ending**: the baseline specs updated only if a contract changed, the change archived.

Not everything needs it. A one-line fix, an exploration, a local experiment: `/aca:delegate` alone.

## Input
$ARGUMENTS

- A slug with a directory under `openspec/changes/` → continue that change at the step its status
  says (below). A step word (`propose`, `apply`, `verify`, `archive`, `status`) overrides that; `Tn`
  picks a task.
- Other text → an idea: **propose**.
- Empty → run the status listing. One change of yours in progress: continue it. Several: show them
  and ask which. None: ask what to build — don't guess.

## First, every time
- Read the root `AGENTS.md` / `CLAUDE.md`, the ones in the directories the change touches, and
  `openspec/config.yaml`.
- Read the **repo profile**, `.claude/spec-profile.md`, if there is one: the gates to run when
  proposing, the checks, how to deploy, how to tell it rolled, how to check it live, the rules every
  brief carries, how a change is archived here. Where it disagrees with this file, it wins. No
  profile: take the checks from `AGENTS.md`, ask the user how to deploy and check live when you get
  there, and offer to write their answers down as the profile.

## Where things go
| what | where | in git |
|---|---|---|
| `proposal.md`, `design.md`, `tasks.md`; `specs/` only when a contract changes | `openspec/changes/<slug>/` | yes — the team reads it |
| briefs, worker results, the verify log, reviews, probes, screenshots | `doc/spec/<slug>/` | no |

Nothing else goes in the change directory: no briefs, per-task results, verify logs or images there.
Check `git check-ignore -q doc/spec/x` first; if `doc/` isn't ignored in this repo, ask where the
working files go.

## Status
`proposal.md` carries one status line, `> 状态：<value>` (or `> Status: <value>` in English):
**Proposed** → **In progress** → **Verified**. The ticks are the `- [ ]` / `- [x]` lines in `tasks.md`.
```bash
for d in openspec/changes/*/; do
  s=$(grep -m1 -E '^> *(状态|Status)[:：]' "$d/proposal.md" 2>/dev/null | sed -E 's/^> *(状态|Status)[:：] *//')
  [ -n "$s" ] || continue
  n=$(grep -cE '^[[:space:]]*- \[[ xX]\]' "$d/tasks.md" 2>/dev/null); x=$(grep -cE '^[[:space:]]*- \[[xX]\]' "$d/tasks.md" 2>/dev/null)
  printf '%-36s %s/%s  %.70s\n' "$(basename "$d")" "${x:-0}" "${n:-0}" "$s"
done
```
A change without that line came from another workflow (the team's, say): leave it alone.

| status | next step |
|---|---|
| Proposed, change files uncommitted | the user hasn't signed off: show the task list, ask to commit and start |
| Proposed, committed | apply; set the status to In progress |
| In progress, boxes open | apply the first open task (or the `Tn` given) |
| In progress, all ticked | verify |
| Verified | archive |

`status` alone prints the listing (or, for one slug, its status, ticks and next step) and stops.

## propose
1. **Evidence first; it decides everything after.** Read the code. Every claim about how things are
   now carries a `file:line` — if you can't give one, you haven't checked, and it stays out. Look in
   your memory for lessons about this area. Run the profile's propose gates. When the design leans
   on runtime mechanics (ordering, state machines, caching, deploy order), have a worker attack it
   through `/aca:delegate` with a read-only brief ("find where this is wrong, with `file:line`") and
   put what it found under 事实来源. Thin evidence → stop and ask, don't write on.
2. **Someone else's change on the same thing** (a `docs/devflow/<slug>/`, or a change directory from
   another workflow): read its PRD as input. Don't write into their directory; pick your own slug.
3. **Write the three files** — templates below. `<slug>` is kebab-case and says what changes; check
   `ls openspec/changes/` for a clash. `specs/` only when the change alters a contract with outside
   consumers (the profile says which); not sure → ask.
4. **Stop.** Report the paths and the task list. Don't start apply.

`proposal.md`:
```markdown
# <title>

> 日期：YYYY-MM-DD
> 状态：Proposed
> 范围：<one sentence: what is in, what is out>

## 一句话目标
## Why            — what is broken or missing, with file:line
## What Changes   — every change point; readable without design.md
## Impact         — modules, tables, interfaces, pages; who gets interrupted
## Non-goals      — what this does not do; it stops the scope drifting later
```

`design.md`:
```markdown
# <title> — 设计
## 事实来源      — what you read (file:line), what a worker checked and what it found
## 现状          — current behaviour, with file:line
## 约束          — repo rules and outside constraints that bind (the profile's gates land here)
## 方案          — down to module / function
## 取舍          — what you considered and didn't pick, and why
## 风险与回滚
```

`tasks.md` — **this is the hand-off**:
```markdown
# <title> — 任务

## 通用边界
<rules every brief carries word for word: the profile's brief rules, plus this change's own>

## T1. <name>
- [ ] 做什么：<concrete change points, down to file / function>
- 验收：<executable — see below>
- 边界：<what not to touch>
```
One task is what one worker can finish coherently in one go: too big and it drifts, too small and
you drown in hand-offs. **验收 is the most important line in the change.** It takes one of three
forms, nothing else: a named test command; a reproducible live step (which environment, what to do,
what you see); a log or metric assertion you can query (what, expected value). Prose like "works
correctly" leaves the worker room to grade its own work.

## apply [Tn]
1. Pick the task: `Tn` if given, else the first with an open box. Run `git status`: the change's files
   should be committed, and the user's uncommitted work isn't in a worker's worktree — don't hand
   off anything that touches it.
2. Write the brief to `doc/spec/<slug>/<slug>-<tn>-plan.md`: the task **verbatim** (acceptance
   included — it is the yardstick), 通用边界 verbatim, and the parts of `design.md` the task needs.
   Add: *do not edit anything under `openspec/changes/<slug>/` (ticks included) or `openspec/specs/`.*
3. **Run it through /aca:delegate** — load it (Claude: the Skill tool, `aca:delegate`; Codex: the
   `delegate` skill) and follow its phases, with the area `spec/<slug>` and the slug `<slug>-<tn>`
   (so its plan is the file above, the worker's result `doc/spec/<slug>/<slug>-<tn>-result.md`, the
   branch `delegate/<slug>-<tn>`). Independent tasks can fan out the same way. Can't load it (not
   installed, or not allowed to start it yourself): stop and ask the user to run
   `/aca:delegate doc/spec/<slug>/<slug>-<tn>-plan.md`.
4. When delegate's verify phase has passed — **you** saw the evidence — tick that task's box, and set
   the status to In progress if it was Proposed. A worker saying it's done is not a tick.
5. Anything under 偏离了计划哪里 that changes the design: update `design.md` yourself, and say it
   out loud there ("上一版的 X 作废，改为 Y") — never a silent rewrite.
6. Offer one commit per task (code + the tick), with the paths named — never `git add -A`; the tree
   may hold someone else's work. Commit when the user says so; "commit each step" said once holds
   for the rest of the change.

## verify
Only when every box is ticked; otherwise name the open ones and stop. Write as you go to
`doc/spec/<slug>/verify.md`: every acceptance line of every task → the command or step → its real
output, pasted → pass / fail.
1. **Checks**: the profile's, else `AGENTS.md`'s "before you finish" commands. Real output, pasted.
2. **Deploy**: it reaches a shared environment, so ask first, every time, naming what will be built
   and where. Then the profile's route.
3. **Did it roll?** Show that the affected pods / workers run the new build (image tag or hash, pod
   age). Code that is right but never rolled out is a whole class of failure — answer this every time.
4. **Live**: run the live acceptance steps with the profile's tools, then read the logs for the run's
   window, errors first.
5. Anything fails → back to apply: reopen the box or add a task. The status stays In progress.
6. All green → `> 状态：Verified（YYYY-MM-DD）`. Report, with the evidence.

## archive
Only when Verified.
1. `specs/` delta → fold it into `openspec/specs/<capability>/` as current behaviour (the baseline
   records how it is, not what was wanted). No delta, no change to `openspec/specs/`.
2. Archive the way the repo says (profile, `AGENTS.md`); without a rule, `openspec archive <slug>` if
   the CLI is there, else move the directory to `openspec/changes/archive/<YYYY-MM-DD>-<slug>/`.
3. Lessons that will matter the next time someone touches this area: into your memory if you keep
   one; otherwise list them in the report.
4. `doc/spec/<slug>/` stays local; say where it is.
5. Ask, then commit.

## Throughout
- Ticks are yours alone, and only after you checked yourself. Workers never edit `proposal.md`,
  `design.md`, `tasks.md` or `openspec/specs/`.
- Every claim about current behaviour carries `file:line`.
- When you reverse an earlier conclusion, write the reversal down.
- Stage by path, never `-A`; the diff holds only this change.
