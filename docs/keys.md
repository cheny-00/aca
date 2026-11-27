# Keys

English | [简体中文](keys.zh-CN.md)

The keys of the interface: in the list, inside a session, and from any tmux session.

Press `?` on any page of the interface for that page's keys: press a key there and it's pressed on the page.
`aca help keys` describes every key in detail.

**In the list**

| Key | What it does |
|---|---|
| `↑` `↓` or `j` `k` | Move. A number jumps to that row |
| `Enter` | Go into the session. A stopped one (`■`) starts first. A Claude router opens a fresh conversation |
| `n` / `N` | New / new with the current settings (same tool, account and directory as the selected session) |
| `i` | Go to the next session waiting for you |
| `Space` | Each session on screen gets a letter; press one to go into that session. Each group gets a capital letter of its own, on the rail: `Shift` + it goes into that group. `Space` again cancels |
| `a` | From inside a group: to all groups, with those letters showing. `a` again goes back to where you were |
| `` ` `` | The cursor onto the rail (on a narrow screen it is laid over the left), with each group's letter showing: press one to put the cursor on that group's row, with its router's exchange beside it. Then `Enter` goes in, `o` into its router, `s` says something to it. `` ` `` again goes back |
| `/` | Filter the list |
| `z` | Read its output full screen, without going in |
| `x` | What it changed: the git diff of the directory it is working in |
| `Z` | The conversation record, turn by turn: what it ran, what it changed, what each turn cost |
| `e` | Write a draft, the next thing to say to it (on a group: a new note in the group's notebook) |
| `s` | Reply without going in: write where `e` writes (its draft already in it), `Enter` sends, `Ctrl+J` is a newline, `Esc` keeps it as the draft. On a `◆` row it answers instead: an option's number, or `y` / `n` for a permission prompt |
| `f` | Fork: a new session that continues from this conversation |
| `H` | Hand off: a new session, with any tool or account, picks up where this one is |
| `X` | Switch accounts when this one runs out of quota. A Claude router starts a new shift on the new account |
| `r` / `R` / `D` | Rename / restart / stop |
| `d` | Delete; on a group, the group with its sessions. Deleted things stay in the trash for a day; `Ctrl+Z` undoes |
| `b` | Trash: what you deleted in the last day (sessions, groups, notes), `⏎` brings it back; `v` selects several to bring back or delete together |
| `m` / `p` | Move to another group / pin to the top |
| `v` `V` | Select several (as in yazi): `v` then move to stretch a range (`Esc` ends it), `V` stretches one that unselects, `Space` selects one; then `d` `D` `R` `m` `u` `p` `Q` act on all of them, `Ctrl+Z` undoes the batch in one step |
| `h` | Onto the rail down the left, to pick a group to show, or all of them (under 130 columns it is laid over the left) |
| `o` | On a group's row: into the group's router (the session that hands the group's work to the right sessions; an `@` leads the group's name), or start one when it has none. The exchange with it is beside the list, and `s` there says something to it. On the rail's All row: into the desk (the scratch group's router, which passes on what you don't know whom to give to), and `s` says something to it the same way. On a session's row, and on a group's tasks and notes pages: that group's router (another group's: `` ` ``, its letter, then `o`) |
| `Tab` | Inside a group: its next page, tasks / sessions / notes. On the tasks page `Enter` opens the session doing the ticket, `s` replies to it, `n` makes a ticket, `e` rewrites it, `m` hands it to a session, `c` marks it done, `d` deletes it; the command line is `aca task`. On the notes page `e` rewrites the note under the cursor, `n` writes a new one, `p` pins it, `d` deletes it; agents read and write the same notes with `aca note` |
| `$` | Usage and cost |
| `P` | Plugins page |
| `?` | This page's keys, each with what it does; on the list your plugins' commands come first. Press one to do it, `/` filters |
| `Ctrl+S` | Settings for theme, language, accounts and logins. Press `c` to toggle all installed external channels. Fill missing settings on the plugins page (`P` from the list) |
| `Ctrl+L` | Restart the interface in place, for example on a newly installed aca |
| `q` `q` | Quit aca. The sessions keep running. In the tmux popup `Esc` closes it too |

A gray `@` marks a stopped router. On its group, `s` wakes a Claude router with your message and `o` opens it.
It stops after `[router] idle` (one hour by default). Every start opens a new conversation with aca's handoff as
the first message. Its role file sets the rules. It has aca's dispatch tools and a question tool, no Bash, and
skips the project's CLAUDE.md and memory. A Codex router keeps its rules in the opening message, stays up when
idle, holds mail while stopped, and resumes the previous conversation with `o`.

When a router dispatches your request, aca carries your words to the worker. Ask it to watch an individual ticket
for updates. Unwatched results appear directly in the exchange. A completion wakes the router only if the ticket
is watched or the group has another ticket to assign.

**Inside a session**

| Key | What it does |
|---|---|
| `Ctrl+Q` | Back to the list. The agent keeps working |
| `Ctrl+Alt+N` | Go straight to the next session waiting for you |
| prefix, then `b` | Pick a session and put its reference (the two lines `c` `b` copies) into the input box, without Enter. Works in the list too: into the highlighted session |

These only apply in sessions that aca started; anywhere else they go to the program as usual (prefix + `b` does
nothing there). In other tmux
sessions, come back with the tmux prefix and `d`.

**From any tmux session, after `aca tmux install`** (optional, see [Optional tools](install.md#optional-tools))

| Key | What it does |
|---|---|
| `Ctrl+\` | Open aca on the right as a switcher; the session beside it follows the highlight |
| prefix, then `a` | Open aca full screen |

**Changing keys.** The keys in the interface itself are fixed. These can be changed:

- `Ctrl+Alt+N`: `aca tmux key next <key>` with a tmux key name such as `M-n` or `F7`. It is saved as
  `[keys] next` in `config.toml`, and `""` turns it off.
- prefix + `b`: `aca tmux key ref <key>`, saved as `[keys] ref`; `prefix b` means `b` after the prefix key, and a key
  without the prefix works too. It isn't Ctrl+Alt+letter by default: window managers (Rectangle, for one) often take
  Ctrl+Option combinations, and the key never reaches tmux.
- `Ctrl+\` and prefix + `a` are in `~/.config/aca/tmux.conf`, which aca rewrites when it is upgraded. To use
  other keys, unbind and bind them in your own `tmux.conf` after the line that loads that file.
- `Ctrl+Q`: if you already bound it in tmux yourself, aca leaves your binding alone; use prefix + `d` instead.
- Plugin keys, in the list's `?` and inside sessions: on the plugins page (`P`) or with `aca plugin hotkey`. See
  [docs/plugins.md](plugins.md).
