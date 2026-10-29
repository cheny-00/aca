//! `aca help`: the overview fits on one screen; the details of a command are `aca help <COMMAND>` (`aca <COMMAND> --help` lands
//! there too).
//!
//! How each command is written is recorded only in this table: its line in the overview, its detail page, and the usage shown
//! when arguments are wrong all come from here.
//! **Adding a command = one branch in `cli.rs` + one entry here**: `cli.rs` doesn't accept names missing from the table, so
//! they never reach its branch.
//!
//! Everything comes in Chinese and English (`Tr::new(chinese, english)`), written side by side: change one, change the other
//! along with it. In English usages, placeholder words are all uppercase
//! (`<TITLE>`, `-g GROUP`), and lowercase ones are words typed as is: completion tells them apart by this (complete.rs), and a
//! test checks that both sides split the same way.

use aca_core::i18n::{Lang, Tr, lang};
use aca_core::tr;

pub struct Cmd {
    /// Names; every name sharing one description is listed (start / stop)
    pub names: &'static [&'static str],
    /// How it's written in the overview (the short form) and its one sentence
    pub short: Tr,
    pub brief: Tr,
    /// The full usage, one form per line, without the leading `aca `
    pub usage: &'static [Tr],
    /// What each argument is, things to know, examples; empty = nothing to add
    pub more: Tr,
}

/// The sections of the overview, in this order
pub const GROUPS: &[(Tr, &[Cmd])] = &[
    (
        Tr::new("界面", "Interface"),
        &[
            Cmd {
                names: &["ui"],
                short: Tr::new("ui [关键词] [-l]", "ui [KEYWORD] [-l]"),
                brief: Tr::new(
                    "界面(即不带参数的 aca);带关键词且只有一个匹配时直接进入",
                    "the interface (plain aca); KEYWORD with one match enters it",
                ),
                usage: &[Tr::new(
                    "ui [关键词] [-l] [-n] [--client 客户端] [--follow]",
                    "ui [KEYWORD] [-l] [-n] [--client CLIENT] [--follow]",
                )],
                more: Tr::new(
                    "\
会话列表 + 高亮会话的实时画面,按回车进入会话;进入后按 Ctrl+Q(或 tmux 前缀键 + d)返回。
在界面里按 ? 查看快捷键;每个键的详细说明:aca help keys

  关键词             只有一个匹配时直接进入,否则打开界面并按它筛选(只在上次所在的工作区里找)
  -l                 只打印列表,不打开界面(输出不是终端时也是这样)
  -n                 只打印会进入哪个会话,不进入
  --client 客户端    在 tmux 弹窗里使用:回车把这个客户端切换过去,打开时高亮它当前所在的会话
  --follow           弹窗只占屏幕一侧时使用(配合 --client):高亮移到哪,这个客户端就切换到哪,弹窗不关闭;
                     ⏎ 留在这里,Esc 返回打开时所在的会话,Ctrl+\\ 关闭。绑定按键的写法:aca help keys
  --snapshot 宽x高 [按键]    不打开终端,按这个尺寸画一帧并打印(测试用)",
                    "\
The list plus a live view of the highlighted session; Enter goes in. Inside, Ctrl+Q (or tmux prefix + d) comes back.
Press ? inside for the keys; every key in detail: aca help keys

  KEYWORD            exactly one match: go straight in; otherwise open the interface filtered by it
                     (searching only the workspace you were last in)
  -l                 only print the list, don't open the interface (also what happens when output isn't a terminal)
  -n                 only print which session it would enter
  --client CLIENT    inside a tmux popup: Enter switches this client over; opens with its current session highlighted
  --follow           when the popup covers one side of the screen (with --client): the client follows the highlight
                     and the popup stays open; ⏎ stays there, Esc goes back where you started, Ctrl+\\ closes it.
                     How to bind it: aca help keys
  --snapshot WxH [KEYS]    draw one frame at that size and print it, without a terminal (for tests)",
                ),
            },
            Cmd {
                names: &["completion"],
                short: Tr::same("completion zsh"),
                brief: Tr::new(
                    "zsh 补全:Tab 补全命令、会话、分组、目录(装了 fzf-tab 就用 fzf)",
                    "zsh completion: commands, sessions, groups, dirs (fzf-tab too)",
                ),
                usage: &[Tr::same("completion zsh")],
                more: Tr::new(
                    "  zsh    打印 zsh 的补全函数(目前只支持 zsh)

两种安装方式任选一种:
  aca completion zsh > ~/.zsh/completions/_aca   放进 fpath 里的某个目录,删除 compinit 的缓存(.zcompdump)后重开 shell
  eval \"$(aca completion zsh)\"                   写进 ~/.zshrc,放在 compinit 之后

补全内容按每条命令的用法来:<id> 补全会话(最近有动静的在前,说明是标题和分组)、-g 分组 补全分组和 .(按目录定)、
[目录] 补全目录、子命令按原样补全;输入 - 后才列出选项。候选项每次都实时向 aca 查询,会话、分组、命令变了都不用重新生成。
装了 fzf-tab 时,在 fzf 里输入标题或组名就能找到会话。要预览会话,在 ~/.zshrc 里加一行
  zstyle ':fzf-tab:complete:aca:*' fzf-preview '[[ $group == *会话* ]] && aca show $word'",
                    "  zsh    print the zsh completion function (zsh is the only one so far)

Install it one of two ways:
  aca completion zsh > ~/.zsh/completions/_aca   a directory on your fpath; delete compinit's cache (.zcompdump), new shell
  eval \"$(aca completion zsh)\"                   in ~/.zshrc, after compinit

What it completes follows each command's usage: <id> completes sessions (most recently active first, described by
title and group), -g GROUP completes groups and . (decided by directory), [DIR] completes directories, subcommands
as written; options show only after you type -. Candidates come fresh from aca on every Tab: nothing to regenerate
when sessions, groups or commands change.
With fzf-tab, type a title or a group name in fzf to find a session. To preview sessions, add to ~/.zshrc
  zstyle ':fzf-tab:complete:aca:*' fzf-preview '[[ $group == *session* ]] && aca show $word'",
                ),
            },
        ],
    ),
    (
        Tr::new("会话", "Sessions"),
        &[
            Cmd {
                names: &["new"],
                short: Tr::new("new <工具[:账号]> [目录]", "new <TOOL[:ACCOUNT]> [DIR]"),
                brief: Tr::new("新建会话并启动(tmux 会话名即标题)", "create a session and start it (tmux session name = title)"),
                usage: &[Tr::new(
                    "new <工具[:账号]> [目录] [-t 标题] [-g 分组] [-w 分支] [--parent id] [--notify 哪些] [--hold] [--say 话] [-- 给 agent 的参数…]",
                    "new <TOOL[:ACCOUNT]> [DIR] [-t TITLE] [-g GROUP] [-w BRANCH] [--parent id] [--notify WHICH] [--hold] [--say TEXT] [-- AGENT-ARGS…]",
                )],
                more: Tr::new(
                    "\
打印新会话的 id(标准输出只有这一行)。

  工具[:账号]    可用的名字:aca names。账号是 config.toml 里登记的名字,也可以直接写配置目录(claude:~/.claude-x);
                 不写账号时用这个工具的默认账号,没有默认账号就什么都不加(使用工具自己的配置目录和登录)
  目录           默认为当前目录
  -t 标题        默认为目录名,之后跟随 agent 自己起的标题;指定了就锁定,不再跟随
  -g 分组        可以带层级(work/app),不存在时自动创建。写 . 表示按目录定(见 aca help group)。
                 不写时:在某个会话里新建的进入它所在的组,否则不分组
  -w 分支        在这个分支的 git worktree 里启动:仓库的 .worktrees/<分支>(已写入 .git/info/exclude,主仓库的
                 git status 里看不到)。分支不存在时从目录当前的 HEAD 创建,存在就直接使用;worktree 的路径写到标准错误。
                 aca rm 时干净的 worktree 一起删除,分支保留;有改动或未跟踪文件的保留。也可以写 --worktree
  --parent id    作为哪个会话的子会话。不写时:在某个会话里运行这条命令就挂在它下面;写 - 表示不挂
  --notify 哪些  在哪些事件上通知 parent(唤醒它):从 done failed exited waiting 中选(逗号分隔),all = 全部,
                 none = 都不通知。不写时按 config.toml 的 [inbox] notify,那里也没写就都不通知(aca help msg)
  --hold         只登记,不启动(之后用 aca start 启动)
  --say 话       开场的第一句话。Claude、Codex、pi、opencode 作为启动参数传入:启动后由它自己发送,这条命令不等待;
                 其他工具等它启动并空闲后再输入(最多等一分钟)
  -- …           每次启动都原样传给 agent 的参数

例:
  aca new claude:work ~/code/app -g app -t 修登录 --say '登录页 500 了,看一下'
  aca new codex . -- --search
  aca new claude -g .            # 当前目录,进入以目录名命名的组",
                    "\
Prints the new session's id (the only line on standard output).

  TOOL[:ACCOUNT]  names it knows: aca names. ACCOUNT is a name registered in config.toml, or just a config directory
                  (claude:~/.claude-x); none given: the tool's default account; no default: nothing is added
                  (it uses its own config directory and login)
  DIR             default: the current directory
  -t TITLE        default: the directory name, then it follows the title the agent gives itself; given: locked
  -g GROUP        can be nested (work/app), created if missing. . = decided by directory (see aca help group).
                  Default: run inside a session, the same group as that session; otherwise no group
  -w BRANCH       start in a git worktree on this branch: the repo's .worktrees/<BRANCH> (listed in .git/info/exclude,
                  so the main checkout's git status doesn't show it). A missing branch starts from the directory's
                  current HEAD, an existing one is used; which worktree goes to standard error. aca rm removes a clean
                  worktree with the session and keeps the branch; one with changes or untracked files stays.
                  Also --worktree
  --parent id     which session it's a child of. Default: run inside a session, that one; - for none
  --notify WHICH  what it tells its parent about (waking it): pick from done failed exited waiting (comma-separated),
                  all = everything, none = nothing. Default: [inbox] notify in config.toml, and if that's unset
                  nothing (aca help msg)
  --hold          register only, don't start (aca start later)
  --say TEXT      the opening message. Claude, Codex, pi and opencode get it as a launch argument: they send
                  it once they're up, and this command doesn't wait; other tools get it typed in once up and idle
                  (waits up to a minute)
  -- …            arguments handed to the agent as they are on every start

Examples:
  aca new claude:work ~/code/app -g app -t fix-login --say 'the login page returns 500, take a look'
  aca new codex . -- --search
  aca new claude -g .            # current directory, into the group named after it",
                ),
            },
            Cmd {
                names: &["adopt"],
                short: Tr::new("adopt [tmux 会话…]", "adopt [TMUX-SESSION…]"),
                brief: Tr::new(
                    "接管不是 aca 启动的 tmux 会话(无需停止);不带参数时列出可接管的",
                    "adopt tmux sessions aca didn't start; bare: list candidates",
                ),
                usage: &[
                    Tr::same("adopt"),
                    Tr::new(
                        "adopt <tmux 会话 | %窗格>… [-t 标题] [-g 分组] [--as 工具[:账号]] [--conv 对话id] [-n] [-- 每次启动带的参数…]",
                        "adopt <TMUX-SESSION | %PANE>… [-t TITLE] [-g GROUP] [--as TOOL[:ACCOUNT]] [--conv CONV-ID] [-n] [-- ARGS…]",
                    ),
                    Tr::new("adopt --all [-g 分组] [-n]", "adopt --all [-g GROUP] [-n]"),
                ],
                more: Tr::new(
                    "\
接管会做三件事:登记一行、在那个 tmux 会话上加标记、把会话名改成标题。里面的进程照常运行,不会往里发送任何按键。
之后它就是普通的 aca 会话:可以重命名、移动分组、停止、原地重启、分叉。不想再管了:aca release <id> 把它移出 aca,进程保留。

  不带参数         列出可直接接管的会话(未登记、第一个窗格中运行着能识别的 claude / codex / pi / opencode),只列出,不改动
  tmux 会话        会话名、名字中的一段(只匹配到一个时)、$会话号;agent 不在第一个窗格时用 %窗格号 指定。可以给多个
  --all            接管列出的全部会话
  -t 标题          不写时:agent-deck 启动的用它那边的标题,其他的用当前的 tmux 会话名(你自己起的名字会被锁定)
  -g 分组          写 . 表示各自按目录定(见 aca help group)。不写时:agent-deck 启动的进入它那边的分组,其他的不分组
  --as 工具[:账号] 不看进程,按这个登记(未识别或识别错时使用)
  --conv 对话id    识别不出对话时手动指定(旧版 Codex 识别不出:在它里面用 /status 查看)
  -n               只显示会怎样登记,不改动
  -- …             每次启动时带的参数;不写就沿用它当前命令行里的参数(用来选择恢复哪段对话的那几个除外)

如何识别:工具看进程名;账号看进程环境里的配置目录,与 config.toml 里登记的对应;Claude 当前的对话和标题读它自己的记录
(配置目录里的 sessions/<pid>.json),Codex 的读它的日志库(按进程号,0.163 起);
agent-deck 启动的,标题、分组、id 按它那边的登记(只读取它的数据库)。
接管后、重启前它没有带上 aca 的 hook:状态只能靠读屏幕,没有「未读」。重启一次(aca restart <id>,对话继续)后就完整了。
agent-deck 启动的例外:它自己的 hook 仍在上报,「未读」借用它的记录,不用等重启。",
                    "\
Adopting = one registry row + a mark on that tmux session + renaming the session to the title. The processes inside
keep running untouched; not a single key is sent in.
From then on it's an ordinary aca session: rename, move, stop, restart in place, fork. Don't want it any more:
aca release <id> lets go, and the processes keep running.

  (no arguments)       list what can be adopted as is (unregistered, a recognizable claude / codex / pi / opencode in the first pane);
                       only looks
  TMUX-SESSION         session name, part of a name (when only one matches), or $session-number; when the agent isn't
                       in the first pane, %pane-number says which pane. Several are fine
  --all                adopt everything listed
  -t TITLE             default: agent-deck's sessions keep its title, others take the current tmux session name
                       (a name you chose yourself gets locked)
  -g GROUP             . = each decided by its directory (see aca help group). Default: agent-deck's go into its
                       group, others get none
  --as TOOL[:ACCOUNT]  ignore the process and register it as this (when not recognized, or recognized wrong)
  --conv CONV-ID       give the conversation when it can't be recognized (an older Codex can't: /status inside)
  -n                   only show how it would be registered; change nothing
  -- …                 arguments for every start; default: copied from its current command line (minus the ones that
                       pick which conversation to resume)

What it recognizes: the agent from the process name; the account from the config directory in the process environment,
matched against config.toml; Claude's current conversation and title from its own records (sessions/<pid>.json in the
config directory), Codex's from its log database (by process id, 0.163 on); for agent-deck's sessions, title, group
and id as registered there (its database is only read).
After adopting and before a restart it carries no aca hooks: state comes from the screen alone, and there is no
\"finished, unread\". Restart once (aca restart <id>, the conversation continues) and it's complete.
agent-deck's are the exception: its own hooks still report, and \"finished, unread\" comes from its records, no restart.",
                ),
            },
            Cmd {
                names: &["ls"],
                short: Tr::same("ls"),
                brief: Tr::new("列表:状态、id、标题、工具:账号、空闲时长", "the list: state, id, title, tool:account, idle time"),
                usage: &[Tr::same("ls [--json] [--no-screen]")],
                more: Tr::new(
                    "  --json         输出 JSON(供脚本读取)
  --no-screen    不读屏幕,只用 hook 上报的状态(几毫秒;hook 不上报的事看不出来,比如按 Esc 中断)

状态:● 运行中  ◆ 等待回应  ◐ 未读  ○ 空闲  ✕ 出错  ■ 已停止  ☾ 自动停止(因空闲被停止)。等待回应的下面多一行它的提问。
在终端里排版并着色:id 只显示前 8 位,输入加粗的那几位就够了(aca attach 3f);最后一行是总数。
输出到管道或文件时不带颜色,id 是完整的。",
                    "  --json         print as JSON (for scripts)
  --no-screen    don't read screens, only the state hooks report (milliseconds; misses what hooks don't report,
                 like an Esc interrupt)

States: ● running  ◆ needs you  ◐ unread  ○ idle  ✕ error  ■ stopped  ☾ auto-stopped (for idling). One that needs you
gets an extra line with what it asked.
In a terminal it's laid out and colored: ids show their first 8 characters, the bold ones are enough to type
(aca attach 3f); the last line is the total. Piped or redirected it's plain, with full ids.",
                ),
            },
            Cmd {
                names: &["show"],
                short: Tr::same("show [id]"),
                brief: Tr::new(
                    "一个会话的详情:状态、目录、对话 id、最后一条回复",
                    "all about one session: state, dir, conversation id, last reply",
                ),
                usage: &[Tr::same("show [id] [--json]")],
                more: Tr::new(
                    "  id        不写时:在某个会话的 tmux 里另开的窗口中运行,就是那个会话
  --json    输出 JSON(供脚本读取)

在终端里最后一条回复按 Markdown 排版、代码按语言着色(aca output 也一样);输出到管道或文件时是原文。",
                    "  id        default: typed in a window you opened inside a session's tmux, that session
  --json    print as JSON (for scripts)

In a terminal the last reply is rendered as Markdown with code highlighted by language (aca output too);
piped or redirected it's the raw text.",
                ),
            },
            Cmd {
                names: &["attach"],
                short: Tr::same("attach <id>"),
                brief: Tr::new(
                    "进入会话(在 tmux 里时切换过去);Ctrl+Q 或 tmux 前缀键 + d 返回",
                    "go in (inside tmux: switch to it); Ctrl+Q or prefix + d leaves",
                ),
                usage: &[Tr::new("attach <id> [--client 客户端]", "attach <id> [--client CLIENT]")],
                more: Tr::new(
                    "  --client 客户端    把这个 tmux 客户端切换过去,停在 agent 那一格(供 tmux 绑定使用,由 tmux 填入 #{client_name})",
                    "  --client CLIENT    switch that tmux client there, onto the agent's pane (for tmux bindings; tmux fills in
                     #{client_name})",
                ),
            },
            Cmd {
                names: &["next"],
                short: Tr::same("next"),
                brief: Tr::new(
                    "进入下一个待处理的会话;在会话里按 Ctrl+Alt+N 也是它",
                    "go to the next session waiting for you; Ctrl+Alt+N inside one",
                ),
                usage: &[Tr::new("next [--session id] [--client 客户端]", "next [--session id] [--client CLIENT]")],
                more: Tr::new(
                    "\
先进入等待你确认 / 选择的(!),再进入已完成但未读的(●),同一类里最近有动静的在前;你当前所在的会话不算。
离开的和进入的都算已读,所以再执行一次会到下一个。没有未读的、只剩等待回应的:在它们之间轮流,最久没去的优先。
一个都没有时提示「暂无待处理会话」,停在原处。

按键:aca 把它绑定在 tmux 上(和 Ctrl+Q 的做法一样,不用前缀,不写入你的 tmux.conf),只在 aca 的会话里拦截,在其他会话里
按键照常发给里面的程序;往上翻看历史时也能按。默认是 Ctrl+Alt+N,记在 config.toml 的 [keys] next;修改:aca tmux key next <键>。

  --session id       你当前所在的会话(按键时由 tmux 填入);不写时:在 tmux 里运行就是运行命令的那个会话
  --client 客户端    切换哪个 tmux 客户端(按键时由 tmux 填入);不写时:运行命令的这个。不在 tmux 里时:连接上去,detach 后返回",
                    "\
Sessions waiting for a confirmation / choice (!) come first, then finished ones you haven't looked at (●), the most
recently active first within each; the one you're in doesn't count. The one you leave and the one you enter both
count as seen, so doing it again goes to the next. When none are unseen and some still ask, it cycles through those,
the one visited longest ago first. Nothing waiting: it says so and you stay put.

The key: aca binds it in tmux (the way Ctrl+Q is: no prefix, nothing written to your tmux.conf), only inside aca's
sessions; elsewhere the key goes to the program as usual. It works while scrolled back too. Ctrl+Alt+N by default,
kept as [keys] next in config.toml; to change it: aca tmux key next <key>.

  --session id       the session you're in (tmux fills it in for the key); default: inside tmux, the one you typed it in
  --client CLIENT    which tmux client to switch (tmux fills it in for the key); default: this one. Outside tmux:
                     attaches, and returns when you detach",
                ),
            },
            Cmd {
                names: &["start", "stop"],
                short: Tr::same("start | stop <id>"),
                brief: Tr::new(
                    "启动(继续原来的对话)/ 停止(agent 退出,窗格和登记保留)",
                    "start, resuming the conversation / stop: the agent exits",
                ),
                usage: &[Tr::same("start <id>"), Tr::same("stop <id>"), Tr::new("stop --idle <时长>", "stop --idle <DURATION>")],
                more: Tr::new(
                    "  stop           让 agent 自行退出(3 秒后仍未退出就强制结束)。它那一格连同最后的画面保留,同一个 tmux 会话里另开的窗口不受影响。
                 列表里显示 ■
  --idle 时长    不写 id:停止空闲超过这个时长的会话(30m、3h、1d、1h30m),列表里显示 ☾,与逐个停止的 ■ 区分。
                 空闲时长从最后一次有动静算起:hook 上报一轮开始 / 结束、有人进入过、子代理的记录有写入;屏幕有输出
                 (工作时的转圈和计时会一直刷新)、对话记录有写入只算一分钟以内的,更早的多半是空闲时的一次重绘
                 (从未上报过 hook 的,比如 shell,多久以前的都算)。以下不会停止:本轮未结束的(hook 只报了开始,屏幕看起来再空闲也不停)、未读的、
                 等待回应的、出错的、有人连着的、有后台任务的、输入框里有未发送文字的、派出去的会话仍未停止的、
                 自己设了定时且还没到点的(Claude 的 CronCreate、/loop 的下一次:只存在于它的进程里,停止后就没了)、
                 收件箱里有还没到点的定时消息的(aca msg --in:停止了到点就送不进去)、
                 重新启动后无法恢复原来对话的。会打印停止了哪些
  start          未运行时在原地重新启动,继续原来的对话(启动后没说过话的会开始新的一段);tmux 里整个不存在了(电脑重启过)就重新创建

自动停止:在 config.toml 里写
  [idle]
  stop_after = \"3h\"
aca 界面打开时每 5 分钟按 stop --idle 3h 执行一次(你连接在某个会话里时也会执行)。",
                    "  stop           ask the agent to exit (forced after 3 seconds). Its pane stays with the last screen; other
                 windows in the same tmux session are left alone. Drawn ■ in the list
  --idle DURATION  without an id: stop the sessions idle this long (30m, 3h, 1d, 1h30m), drawn ☾ in the list,
                 apart from the ■ of ones stopped one by one. Idle counts from the last sign of life: a hook reporting
                 a turn starting or finishing, someone attaching, a write to a subagent's record; output on screen (a
                 working agent's spinner and timer keep redrawing) and writes to its conversation record count only
                 within the last minute, as an older one is most likely an idle redraw (all of them count for one that
                 never reported a hook, like a shell). Never stopped: a turn that hasn't finished (the hook reported
                 only its start, however idle the screen looks), finished-unread, waiting for you, failed, attached,
                 background tasks, unsent text in its input box, dispatched sessions still open, a schedule of its own
                 not yet due (Claude's CronCreate, /loop's next wakeup: they live only in its process), a scheduled
                 message not yet due in its inbox (aca msg --in: stopped, it couldn't be delivered on time), or one
                 that can't resume its conversation. Prints the ones it stopped
  start          not running: start it again in place, resuming the conversation (one that never talked starts fresh);
                 gone from tmux entirely (after a reboot): create it again

To stop them automatically, put in config.toml
  [idle]
  stop_after = \"3h\"
and while the aca interface is open it does a stop --idle 3h every 5 minutes (also while you're inside a session).",
                ),
            },
            Cmd {
                names: &["restart"],
                short: Tr::same("restart <id>"),
                brief: Tr::new(
                    "原地重启并继续对话;--fresh 开始新对话;--as 换账号继续",
                    "restart in place, resuming; --fresh anew, --as other account",
                ),
                usage: &[Tr::new("restart <id> [--fresh] [--as 工具[:账号]]", "restart <id> [--fresh] [--as TOOL[:ACCOUNT]]")],
                more: Tr::new(
                    "  --fresh          不恢复原来的对话,开始新的对话
  --as 工具[:账号] 换一个账号继续这段对话(一个账号额度用完了、任务还没做完):对话记录从原账号的配置目录复制到
                   新账号的相同位置,上下文完整带过去,列表行、tmux 会话、标题都不变;原账号的那份保留,之前几轮
                   的费用仍算在原账号上。只写一个词时:是这个工具的账号就当账号,否则当工具名(用它的默认账号)。
                   只能在同一种工具内切换(Claude 和 Codex 的对话记录不互通,换工具请用界面里的 H 交接)。
                   Codex 各账号默认模型不同时会带上原来的 -m。新账号的缓存是空的:第一轮会重新写入整段上下文",
                    "  --fresh              don't resume the conversation, start a new one
  --as TOOL[:ACCOUNT]  carry this conversation over to another account (one ran out of quota mid-task): its
                       record is copied from the old account's config directory to the same place in the new
                       one, so the context comes along whole and the row, tmux session and title stay; the old
                       account keeps its copy and the earlier turns still bill to it. A single word is an
                       account of this tool if there is one, otherwise a tool (with its default account).
                       Only within one family (Claude and Codex conversations don't mix; to change family, hand
                       off with H in the UI). Codex accounts with different default models get the old -m
                       carried. The new account's cache is cold: the first turn writes the whole context again",
                ),
            },
            Cmd {
                names: &["fork"],
                short: Tr::same("fork <id>"),
                brief: Tr::new(
                    "从当前对话分叉出一个新会话(分组、账号与原会话相同,排在它后面)",
                    "branch a new session off its conversation, listed after it",
                ),
                usage: &[Tr::new("fork <id> [-t 标题]", "fork <id> [-t TITLE]")],
                more: Tr::new(
                    "  -t 标题    默认为「原标题 (分叉)」

打印新会话的 id。原会话要有过对话才能分叉。
分叉出的会话是你自己继续聊的另一条线,与原会话平级:不算原会话派出去的(没有 parent,不会通知它),来源记在 fork_of。",
                    "  -t TITLE    default: \"<title> (fork)\"

Prints the new session's id. Same group and account as the original. Needs a conversation to fork.
The fork is another line of your own, a peer of the original: not one it dispatched (no parent, it isn't
notified); where it came from is recorded as fork_of.",
                ),
            },
            Cmd {
                names: &["rename"],
                short: Tr::new("rename <id> <标题>", "rename <id> <TITLE>"),
                brief: Tr::new("重命名:登记、tmux 会话名、窗口名一起修改", "rename: registry, tmux session and window names together"),
                usage: &[Tr::new("rename <id> <标题>", "rename <id> <TITLE>"), Tr::same("rename <id> --auto")],
                more: Tr::new(
                    "  --auto    解除锁定,重新跟随 agent 自己起的标题

重命名后名字会被锁定,不再跟随 agent 自己起的标题。",
                    "  --auto    release the name and follow the agent's own title again

Once renamed the name is locked and no longer follows the agent's own title.",
                ),
            },
            Cmd {
                names: &["move"],
                short: Tr::new("move <id> <分组>", "move <id> <GROUP>"),
                brief: Tr::new("移到另一个分组(不存在时自动创建),排在该组最后", "move to another group (created if missing), at its end"),
                usage: &[Tr::new("move <id> <分组>", "move <id> <GROUP>")],
                more: Tr::new(
                    "分组写 '' 表示移出分组;写 . 表示按它的目录定的组(见 aca help group),会打印移到了哪个组。",
                    "GROUP '' moves it out of any group; . is the group its directory decides (see aca help group), printed.",
                ),
            },
            Cmd {
                names: &["order"],
                short: Tr::new("order <id> <位置>", "order <id> <POS>"),
                brief: Tr::new("在组内的顺序(一个数,小的在前)", "position within its group (a number, smaller first)"),
                usage: &[Tr::new("order <id> <位置>", "order <id> <POS>")],
                more: Tr::same(""),
            },
            Cmd {
                names: &["group"],
                short: Tr::same("group ls | add | rename | dir | …"),
                brief: Tr::new("分组(可以带层级:work/app)", "groups (can be nested: work/app)"),
                usage: &[
                    Tr::same("group ls [--json]"),
                    Tr::new("group add [目录]", "group add [DIR]"),
                    Tr::new("group create <路径> [默认目录]", "group create <PATH> [DEFAULT-DIR]"),
                    Tr::new("group rename <路径> <新路径>", "group rename <PATH> <NEW-PATH>"),
                    Tr::new("group dir <路径> [目录 | '']", "group dir <PATH> [DIR | '']"),
                    Tr::new("group rm <路径>", "group rm <PATH>"),
                    Tr::new("group order <路径> <位置>", "group order <PATH> <POS>"),
                ],
                more: Tr::new(
                    "  ls        全部分组:顺序、路径、默认目录
  --json    输出 JSON(供脚本读取)
  add       把目录(默认为当前目录)登记为分组,组名按目录定(见下文),并打印组名;已有对应的组时只打印组名
  create    自己起名,上级分组一并创建。默认目录:在界面里往这个组新建会话时预先填入的路径
  rename    重命名,或者移到别的上级下面(fun/aca);子分组和组内的会话随之移动,界面里记着的状态(工作区、折叠)也一并更新。
            在界面里选中分组按 r 也是它,之后还能修改默认目录
  dir       查看 / 修改默认目录,'' 表示清除。一个目录只能作为一个组的默认目录
  rm        子分组一并删除;组内的会话不会被停止或删除,而是移到上一级
  order     分组的顺序(一个数,小的在前)

按目录定组(group add,以及 new / adopt / move 里的 -g . 或分组写 .;在界面里按 N 改了路径时也按这套规则):
  1. 某个组的默认目录就是它 → 那个组
  2. 目录名已经是分组(同名的顶层组,或唯一一个「…/目录名」的嵌套组)→ 那个组
  3. 上一级目录名是分组 → 嵌套在它下面:「那个组/目录名」
  4. 以上都不是 → 顶层的「目录名」
  2–4 定出的组如果记着别的目录 → 依次编号:…-2、-3。定出的组没有默认目录时,把这个目录记为默认目录。
  名字不合适:用 aca group rename 修改

把会话放进分组:aca new … -g <路径>、aca move <id> <路径>。",
                    "  ls        all groups: position, path, default directory
  --json    print as JSON (for scripts)
  add       register a directory (default: the current one) as a group named by the rules below, print its name;
            if a group already has it, just print that one
  create    name it yourself; parents are created too. Default directory: the path prefilled when you create a
            session in this group from the interface
  rename    rename, or move under another parent (fun/aca); subgroups and sessions follow, and what the interface
            remembers about it (workspace, folding) follows too. Same as r on a group in the interface, which then
            also lets you change the default directory
  dir       show / change the default directory; '' = forget it. A directory is the default of one group at most
  rm        deletes subgroups too; sessions inside aren't stopped or deleted, they move up a level
  order     group position (a number, smaller first)

Groups by directory (group add, and -g . or GROUP . in new / adopt / move; N in the list with a
changed path follows the same rules):
  1. the group whose default directory it is → that group
  2. the directory's name is a group (top-level, or the only nested …/<name>) → that group
  3. the parent directory's name is a group → nested under it: <group>/<name>
  4. otherwise the top-level \"<dir name>\"
  If the group from 2–4 has another default directory → numbered: …-2, -3. If the chosen group has no
  default directory, this one is recorded. Don't like the name: aca group rename

Putting sessions in a group: aca new … -g <PATH>, aca move <id> <PATH>.",
                ),
            },
            Cmd {
                names: &["send"],
                short: Tr::new("send [id] -m <话>", "send [id] -m <TEXT>"),
                brief: Tr::new("在它的输入框里输入一段话并回车", "type text into its input box and press Enter"),
                usage: &[Tr::new("send [id] [-m 话] [--wait 秒]", "send [id] [-m TEXT] [--wait SECS]")],
                more: Tr::new(
                    "  id           不写时:在某个会话的 tmux 里另开的窗口中运行,就是那个会话
  -m 话        不写时读标准输入(可以多行:作为粘贴输入,不会被当成发送了几次)
  --wait 秒    它正在运行 / 等待回应时先等它空闲,最多等这么久,到时仍未空闲就不发送(退出码 1);不写时不等待,直接发送

未运行的会话无法发送。不带 --wait 时如果它在等待确认:不输入(回车可能直接选中默认选项),报错;要替它回答请用 aca answer。
给别的会话传话(不动它的输入框、它不方便时先保存)请用 aca msg。",
                    "  id             default: typed in a window you opened inside a session's tmux, that session
  -m TEXT        default: read standard input (several lines are fine: it's pasted, not sent several times)
  --wait SECS    running / waiting for you: wait until it's idle, up to this long; still busy then: don't send
                 (exit code 1). Default: no waiting, send right away

Can't send to one that isn't running. Without --wait, one waiting for a confirmation isn't typed into (Enter might
pick the default option): it's an error; to answer for it use aca answer.
To pass a message to another session (without touching its input box, kept until it's convenient) use aca msg.",
                ),
            },
            Cmd {
                names: &["output"],
                short: Tr::same("output [id]"),
                brief: Tr::new("最后一条回复(由 hook 记录,最长 4000 字)", "the last reply (recorded by hooks, up to 4000 characters)"),
                usage: &[Tr::same("output [id]")],
                more: Tr::new("  id        不写时:在某个会话的 tmux 里另开的窗口中运行,就是那个会话", "  id        default: typed in a window you opened inside a session's tmux, that session"),
            },
            Cmd {
                names: &["seen"],
                short: Tr::same("seen <id>"),
                brief: Tr::new("标为已读(◐ 未读 → ○)", "mark as seen (◐ unread → ○)"),
                usage: &[Tr::same("seen <id>")],
                more: Tr::same(""),
            },
            Cmd {
                names: &["archive", "unarchive"],
                short: Tr::same("archive | unarchive <id>"),
                brief: Tr::new(
                    "归档(关闭 tmux 会话,保留登记,不再显示在列表里)/ 恢复",
                    "archive (tmux session closed, row kept, unlisted) / restore",
                ),
                usage: &[Tr::same("archive <id>"), Tr::same("unarchive <id>")],
                more: Tr::new(
                    "恢复后处于已停止状态:aca start <id> 启动并继续原来的对话。",
                    "Restored it's stopped: aca start <id> brings it up resuming the conversation.",
                ),
            },
            Cmd {
                names: &["release"],
                short: Tr::same("release <id>"),
                brief: Tr::new(
                    "移出 aca:只删除登记,tmux 会话和其中的进程保留(adopt 的反操作)",
                    "let go: unregister only, tmux and processes stay (un-adopt)",
                ),
                usage: &[Tr::same("release <id>")],
                more: Tr::new(
                    "它会变回普通的 tmux 会话(会话名不改回原来的)。要重新接管:aca adopt。",
                    "It's an ordinary tmux session again (the name isn't changed back). Want it back: aca adopt.",
                ),
            },
            Cmd {
                names: &["rm"],
                short: Tr::same("rm <id>"),
                brief: Tr::new(
                    "删除:关闭 tmux 会话(连同另开的窗口),删除登记和状态",
                    "delete: close its tmux session (all windows), drop its records",
                ),
                usage: &[Tr::same("rm <id>")],
                more: Tr::new(
                    "对话记录归 agent 自己管理,不会删除。只想删除登记、让它继续运行:aca release。",
                    "Conversation records are the agent's own and stay. To only unregister and keep it running: aca release.",
                ),
            },
        ],
    ),
    (
        Tr::new("会话之间传话", "Between sessions"),
        &[
            Cmd {
                names: &["msg"],
                short: Tr::new("msg <id | parent> <话>", "msg <id | parent> <TEXT>"),
                brief: Tr::new(
                    "给别的会话传一句话:放进它的收件箱,在它方便时送达",
                    "pass a message: into its inbox, delivered when it's convenient",
                ),
                usage: &[Tr::new("msg [--in 时长] <id | parent> [话…]", "msg [--in DURATION] <id | parent> [TEXT…]")],
                more: Tr::new(
                    "\
  id             发给谁:会话 id 或名字
  parent         派你出来的那个会话(在某个会话里发送时)
  话             不写时读标准输入
  --in 时长      过这么久再送(90s、10m、1h30m,最长 7 天);定时的可以发给自己,到点叫醒自己。
                 没到点的在 aca inbox 里是 [定时],列表上标 ◷;有它的闲着也不会被自动停
在某个会话里发送的,发件人是那个会话;在外面发送的,发件人是「你」。

送达方式取决于对方的工具,都不动它的输入框:
  Claude    按门铃:它自己监视一个文件,文件一变就取走消息,作为系统提示交给模型。空闲时立即开始一轮,
            运行中就并入这一轮,等待确认时等确认完再送达
  Codex     放进它自己的队列(codex queue):空闲时 10 秒内自己开始一轮,运行中就等这一轮结束。
            你刚按 Esc 中断它时不去催,等你的下一轮结束后再送达
  输入      以上两种都用不了时(接管来的、没接上门铃的 Claude;还没说过话的 Codex;其他 agent):
            只在它空闲、你没在那一格输入、输入框里没有你未发送的文字时才输入,否则过一会儿再试
暂时无法送达的先保存,等它这一轮结束或重新启动时再送达。shell 会话不接收消息。
放进队列或输入进去的就是那句话:你发的保持原样,别的会话发的前面多一行「[aca] 来自会话 …」,不会往 agent 里加别的内容。

派出会话时要求了通知(aca new --notify done,或 config.toml 的 [inbox] notify)的,被派出的会话在一轮结束、
出错、退出、等待确认时,它自己的 hook 会给 parent 发一条通知,不需要有人一直盯着。没要求就不发:不会唤醒任何会话。
同一个会话 10 分钟内给同一个会话发了超过 20 条,之后的只保存、不唤醒。",
                    "\
  id             who to: a session id or a name
  parent         the one that dispatched you (when sent from inside a session)
  TEXT           default: read standard input
  --in DURATION  deliver it after this long (90s, 10m, 1h30m; 7 days at most); a scheduled one can go to
                 yourself, to wake you up then. Until then it's [scheduled] in aca inbox and ◷ in the list,
                 and the session isn't stopped for being idle
Sent from inside a session, the sender is that session; from outside it's \"you\".

How it gets in depends on the agent; none of them touch its input box:
  Claude    rings a doorbell: it watches a file itself and picks the message up as a system note for the model.
            Idle: a turn starts right away; running: it joins this turn; waiting for a confirmation: after that
  Codex     goes into its own queue (codex queue): idle, it starts a turn within 10 seconds; running, after this turn.
            Right after you press Esc to interrupt it, no nudging: delivered after your next turn finishes
  typing    when neither works (an adopted Claude without the doorbell; a Codex that hasn't talked yet; other agents):
            only typed when it's idle, you're not typing in that pane and its input box has nothing unsent
What can't be delivered is kept and delivered when this turn ends or it starts again. Shell sessions don't take any.
Queued or typed, it's that text and nothing else: yours as written, another session's with a line
\"[aca] from session …\" in front. Nothing else is added to the agent.

When asked for at dispatch (aca new --notify done, or [inbox] notify in config.toml), a dispatched session's own
hooks notify its parent when a turn finishes, fails, it exits or it waits for a confirmation, so nobody has to keep
watching. Not asked for: nothing is sent, nobody is woken.
More than 20 from one session to another within 10 minutes: the rest are kept, nobody is woken.",
                ),
            },
            Cmd {
                names: &["inbox"],
                short: Tr::same("inbox [id]"),
                brief: Tr::new(
                    "收件箱:收到了什么、是否已送达;不给 id 时列出有未送达消息的会话",
                    "inbox: what came, delivered or not; no id: who has undelivered",
                ),
                usage: &[Tr::new("inbox [id] [-n 条数] [--json]", "inbox [id] [-n COUNT] [--json]")],
                more: Tr::new(
                    "  id        不写时:在某个会话里就是它自己的;在外面就列出哪些会话有未送达的消息
  -n 条数   最近几条(默认 20)
  --json    一行一条 JSON(供脚本读取)

[定时 10:20] 还没到点  [未送达] 还在收件箱里  [排队中] 已放进 Codex 的队列,它还没为这条开始一轮  [已送达] 已交给它",
                    "  id         default: inside a session, its own; outside, which sessions have undelivered messages
  -n COUNT   the latest few (default 20)
  --json     one JSON message per line (for scripts)

[scheduled 10:20] not due yet  [undelivered] still in the inbox  [queued] in Codex's queue, no turn for it yet
[delivered] in front of it",
                ),
            },
            Cmd {
                names: &["wait"],
                short: Tr::same("wait <id>…"),
                brief: Tr::new(
                    "等待它一轮结束 / 等待确认 / 出错 / 退出;退出码表示是哪种",
                    "wait for a turn to end / a confirmation / failure / exit",
                ),
                usage: &[Tr::new("wait <id>… [--any] [--timeout 秒]", "wait <id>… [--any] [--timeout SECS]")],
                more: Tr::new(
                    "  --any          等到一个就返回;不写时等全部
  --timeout 秒   最多等这么久(超时退出码 124)

退出码:0 一轮结束  2 等待确认  3 出错  4 已退出 / 已停止  124 超时。同时等多个时,取第一个非 0 的。
出错的那一行会写明类别:临时性的(限流、过载、断流:过一会儿用 aca send 让它继续)、账号问题、重试也无效的。
Codex 一轮出错时不上报 hook:aca 等待时从它的对话记录里读取。
刚发给它的话还没送达、还没进入「运行中」的,不算一轮结束。
在某个会话里调用时:等到的那几个会话发来的通知记为已送达,不会再因为它们被唤醒一次。",
                    "  --any            return on the first one; default: wait for all
  --timeout SECS   wait at most this long (exit code 124)

Exit codes: 0 a turn finished  2 waiting for a confirmation  3 failed  4 exited / stopped  124 timed out.
Waiting on several: the first one that isn't 0.
Text just sent to it that hasn't arrived, or hasn't turned it into \"running\" yet, doesn't count as finished.
Called from inside a session: the notifications from the ones it waited for are marked delivered,
so it isn't woken for them again.",
                ),
            },
            Cmd {
                names: &["answer"],
                short: Tr::new("answer <id> [回答]", "answer <id> [ANSWER]"),
                brief: Tr::new(
                    "替等待确认的会话回答:选择题按选项号,权限框 yes / no",
                    "answer for one waiting: a choice's number, or yes / no",
                ),
                usage: &[Tr::same("answer <id>"), Tr::new("answer <id> <选项号 | yes | no>", "answer <id> <NUMBER | yes | no>")],
                more: Tr::new(
                    "\
不带回答:只显示它在问什么、有哪些选项,或者要运行什么命令。
  选项号      选择题(一次只问一题、单选的):按下那个数字
  yes         在权限框里允许:Claude 按 1
  no          在权限框里拒绝:Claude 按 Esc。权限框的选项个数和顺序会变,所以只支持这两种
  选项号      启动时的确认框(信任这个目录、有更新、hook 待审核这类只出现在屏幕上的):屏幕上有一组带编号的选项时才有效,
              用方向键移到那一项再回车

按键前先核对它问的还是同一个问题,不是就不按。按完在事件流里记一条 ask.answered(谁替谁回答的、回答了什么)。
在某个会话里调用时,只能回答它自己派出去的会话(parent 是它,或更下层的)。",
                    "\
No answer: only shows what it's asking, the options, or the command it wants to run.
  NUMBER      a choice (one question at a time, single choice): press that number
  yes         allow in a permission prompt: Claude gets 1
  no          deny in a permission prompt: Claude gets Esc. The number and order of a permission prompt's
              options change, so only these two
  NUMBER      a startup gate (trust this folder, an update, hooks to review: boxes seen only on screen):
              only when the screen shows numbered options; arrows walk to that one, then Enter

Before pressing it checks it's still asking the same thing; if not, nothing is pressed. Afterwards an
ask.answered event is logged (who answered for whom, and what).
Called from inside a session, it can only answer the ones it dispatched (its children, or further down).",
                ),
            },
            Cmd {
                names: &["interrupt"],
                short: Tr::same("interrupt [id]"),
                brief: Tr::new(
                    "中断它正在运行的这一轮(按 Esc):方向不对的时候用",
                    "stop its current turn (Esc): when it went the wrong way",
                ),
                usage: &[Tr::same("interrupt [id]")],
                more: Tr::new(
                    "\
id 不写时:在某个会话的 tmux 里另开的窗口中运行,就是那个会话。
只在它确实在运行时才按;等待确认时不按(Esc 在确认框里表示拒绝,要拒绝请用 aca answer <id> no)。
中断后它处于空闲:要换个说法让它继续,用 aca send <id> -m '…'(输入后立即开始一轮)。
Codex 被中断后,它队列里的消息不会自动开始执行,也要用 aca send。
在某个会话里调用时,只能中断它自己派出去的会话。",
                    "\
id defaults to the session whose tmux you typed this in (in a window you opened there).
Only pressed while it's really running; not when it waits for a confirmation (there Esc means no; to say no
use aca answer <id> no).
Once stopped it's idle: to have it carry on with different instructions use aca send <id> -m '…' (typed in,
a turn starts right away). After Codex is interrupted, what's in its queue doesn't start by itself either: use aca send.
Called from inside a session, it can only stop the ones it dispatched.",
                ),
            },
        ],
    ),
    (
        Tr::new("账号和工具", "Accounts and tools"),
        &[
            Cmd {
                names: &["names"],
                short: Tr::same("names"),
                brief: Tr::new("new 可用的名字:工具,以及「工具:账号」", "names new accepts: tools, and TOOL:ACCOUNT"),
                usage: &[Tr::same("names")],
                more: Tr::new(
                    "账号和工具在 config.toml 里登记,写法见 aca help config。",
                    "Accounts and tools are registered in config.toml; how to write it: aca help config.",
                ),
            },
            Cmd {
                names: &["account"],
                short: Tr::new("account add <claude|codex> <名字>", "account add <claude|codex> <NAME>"),
                brief: Tr::new(
                    "登记账号(名字 = 配置目录);status 查看登录状态,rm 删除登记",
                    "register an account (name = config dir); also status, rm",
                ),
                usage: &[
                    Tr::same("account ls"),
                    Tr::same("account status"),
                    Tr::new(
                        "account add <claude | codex> <名字> [目录] [--statusline aca|wrap]",
                        "account add <claude | codex> <NAME> [DIR] [--statusline aca|wrap]",
                    ),
                    Tr::new("account rm <claude | codex> <名字> [--force]", "account rm <claude | codex> <NAME> [--force]"),
                ],
                more: Tr::new(
                    "  ls              已登记的账号:工具:名字、配置目录(不带参数时也是它)。顺序与界面账号页里的排列一致
  status          各账号是否已登录:询问工具自己(claude auth status / codex login status),aca 不读取凭据;
                  配置目录不存在、目录其实属于另一种工具(把 ~/.codex 登记成了 Claude 的账号)也会在这里提示。
                  不指定账号时用的那个(已安装工具自己的 ~/.claude、~/.codex)也会列出
  add             登记一个账号:在 config.toml 的 [accounts.claude] / [accounts.codex] 下加一行「名字 = 目录」,
                  其他内容和注释不变;目录不存在时自动创建。目录是另一种工具的配置目录时拒绝登记
  rm              删除一个登记:只去掉 config.toml 里的那一行(删除前的文件保存为 config.toml.bak-aca),
                  配置目录和其中的登录都不变。有会话在用、或是某个工具的默认账号时不删除,并提示谁在用
  --force         rm:有会话在用、是默认账号也删除
  claude          Claude 的账号(启动会话时这个目录作为它的 CLAUDE_CONFIG_DIR)
  codex           Codex 的账号(这个目录作为它的 CODEX_HOME)
  名字            自己起(字母、数字、- 和 _),启动会话时写成「claude:名字」
  目录            它的配置目录;默认为 ~/.claude-<名字> / ~/.codex-<名字>
  --statusline    仅限 Claude 账号:登记后顺便接上它的状态栏(不写就不接)
  aca             状态栏由 aca statusline 绘制
  wrap            保留它自己的状态栏,外面套一层 aca limits,只记录额度

aca 不扫描、不猜测有哪些账号,也不动登录:新建的目录需要用工具自己的登录方式登录一次,命令会打印出来
(aca run claude:<名字>,Codex 是 aca run codex:<名字> -- login)。不登记也能用:就是 ~/.claude、~/.codex。
想在一张表里选:aca setup。界面里是设置页(^S)的 a:登录状态、添加、删除、⇧↑ ⇧↓ 排序都在那一页。",
                    "  ls              registered accounts: tool:name, config directory (also with no arguments), in the order
                  set on the interface's accounts page
  status          whether each account is logged in: asks the tool itself (claude auth status / codex login status),
                  aca reads no credentials; also says when a config directory is missing or belongs to the other
                  tool (~/.codex registered as a Claude account). The one used when you name no account (an
                  installed tool's own ~/.claude or ~/.codex) is listed too
  add             register one: adds a \"name = directory\" line under [accounts.claude] / [accounts.codex] in
                  config.toml, leaving everything else and the comments alone; creates the directory if missing.
                  A directory that is the other tool's config directory is refused
  rm              unregister one: removes only its line in config.toml (the file as it was is kept as
                  config.toml.bak-aca); the config directory and the login in it stay. If sessions use it or it's
                  a tool's default account, nothing is removed and it says who uses it
  --force         rm: remove even when sessions use it or it's a default account
  claude          a Claude account (the directory is its CLAUDE_CONFIG_DIR when sessions start)
  codex           a Codex account (the directory is its CODEX_HOME)
  NAME            yours to choose (letters, digits, - and _); start sessions with \"claude:NAME\"
  DIR             its config directory; default ~/.claude-<NAME> / ~/.codex-<NAME>
  --statusline    Claude accounts only: wire up its status bar right after registering (default: don't)
  aca             the status bar is drawn by aca statusline
  wrap            keep its own status bar, wrapped in aca limits just to record quotas

aca doesn't scan for or guess accounts, and doesn't touch logins: a new directory needs one login with the tool's
own login, and the command is printed (aca run claude:<NAME>; for Codex aca run codex:<NAME> -- login).
It works without registering too: that's ~/.claude, ~/.codex.
To pick from a table: aca setup. In the interface it's a on the settings page (^S): login state, adding, removing and
ordering (⇧↑ ⇧↓) are all on that page.",
                ),
            },
            Cmd {
                names: &["run"],
                short: Tr::new("run <工具[:账号]> -- …", "run <TOOL[:ACCOUNT]> -- …"),
                brief: Tr::new(
                    "用这个账号的环境运行工具自己的命令(login、mcp …),不新建会话",
                    "run the tool's own commands (login, mcp …) as that account",
                ),
                usage: &[Tr::new("run <工具[:账号]> [-- 参数…]", "run <TOOL[:ACCOUNT]> [-- ARGS…]")],
                more: Tr::new(
                    "\
等同于在终端里输入 CODEX_HOME=~/.codex2 codex login,只是不用再输入一遍目录。
带上的是账号的环境;参数只有你给的(工具、账号里登记的 args 用于启动会话,这里不带)。
运行登录(claude 的 auth login、codex login)时不会替你打开默认浏览器:
链接由工具自己打印在屏幕上,复制到要用的浏览器里;你自己设置了 BROWSER 时,Claude 会使用它。
通过 SSH 连接时改用能在其他设备上完成的登录方式:Claude 给出需要把验证码粘贴回终端的链接,Codex 加上 --device-auth。
在界面的账号页按 ⏎ 登录,出现链接时会弹出一张表:复制链接,或选择一个浏览器打开。

例:
  aca run codex:team -- login
  aca run claude:work -- mcp list",
                    "\
Same as typing CODEX_HOME=~/.codex2 codex login in a terminal, without typing the directory again.
It brings the account's environment; the only arguments are yours (args registered on the tool or account are for
starting sessions and aren't added here).
A login (claude's auth login, codex login) doesn't open your default browser: the tool prints the link, paste it
into the browser you want; if you set BROWSER yourself, Claude uses yours.
Over SSH it switches to a login you can finish on another device: Claude gives the link whose page shows a code to
paste back, Codex gets --device-auth.
⏎ on the accounts page in the interface pops up a list once the link comes: copy it, or pick a browser to open it.

Examples:
  aca run codex:team -- login
  aca run claude:work -- mcp list",
                ),
            },
            Cmd {
                names: &["import"],
                short: Tr::same("import agent-deck"),
                brief: Tr::new(
                    "导入 agent-deck 登记的分组和会话(只读取它的数据库)",
                    "bring over agent-deck's groups and sessions (read-only)",
                ),
                usage: &[Tr::same("import agent-deck [--apply] [--all]")],
                more: Tr::new(
                    "  agent-deck    从哪里导入(目前只支持它)
  --apply       实际写入;不写时只列出会导入哪些
  --all         那边仍在运行的也登记(登记为已停止:避免两边同时打开同一段对话);不写时跳过

导入的是登记信息(标题、分组、顺序、账号、对话 id),id 沿用那边的;导入后处于已停止状态,aca start <id> 启动并继续对话。
那边仍在运行的:要么先在那边停止再导入,要么不停止、直接接管(aca adopt)。",
                    "  agent-deck    where from (the only one it knows for now)
  --apply       actually write them; default: only list what would come over
  --all         also register the ones still running there (as stopped: don't open one conversation on both
                sides); default: skip them

It brings the registrations (title, group, order, account, conversation id), keeping agent-deck's ids; they arrive
stopped, and aca start <id> resumes the conversation.
Ones still running there: stop them there and import, or take them over without stopping (aca adopt).",
                ),
            },
        ],
    ),
    (
        Tr::new("用量、配色、对外", "Usage, colors, beyond this machine"),
        &[
            Cmd {
                names: &["cost"],
                short: Tr::same("cost [1|7|30|all]"),
                brief: Tr::new(
                    "费用:本机 Claude / Codex 用量按 API 价格折成美元,以及订阅额度",
                    "spend: local Claude / Codex usage at API prices, plus quotas",
                ),
                usage: &[Tr::new("cost [1|7|30|all] [--only 账号,…]", "cost [1|7|30|all] [--only ACCOUNT,…]")],
                more: Tr::new(
                    "  1         今天
  7         近 7 天(默认)
  30        近 30 天
  all       全部记录(需要读取一两秒)
  --only    只统计这几个账号(逗号分隔,也可以写多次)

与界面里的 $ 相同。只读取本地的对话记录,不联网。",
                    "  1         today
  7         the last 7 days (the default)
  30        the last 30 days
  all       all records (takes a second or two)
  --only    only these accounts (comma-separated, or give it several times)

Same as $ in the interface. Reads local conversation records only, no network.",
                ),
            },
            Cmd {
                names: &["limits"],
                short: Tr::new("limits [-- 命令…]", "limits [-- COMMAND…]"),
                brief: Tr::new(
                    "供 Claude 的 statusLine 使用:记录额度,输入原样传给后面的命令",
                    "for Claude's statusLine: record quotas, pass input to COMMAND",
                ),
                usage: &[Tr::new("limits [-- 命令…]", "limits [-- COMMAND…]"), Tr::same("limits --json")],
                more: Tr::new(
                    "\
想保留自己的状态栏时使用:在 Claude 的 settings.json 里把 statusLine 的命令写成「aca limits -- 原来的命令」。
记录的额度显示在计费页顶部。状态栏也交给 aca 绘制的话,用 aca statusline,不用这条。
按模型区分的周额度(Fable)不在状态栏里:账号的数字有变化时,至少间隔五分钟,在后台启动 aca limits --ask 询问 Claude 自己
(claude -p 的 get_usage,与 /usage 同一来源),只询问已登记的账号。

不经过状态栏的记录方式:在界面 ^S 里按 m 开启(config.toml 的 [tools.claude] mod = true),aca 启动 Claude 时带上自己的 mod,
由它在 Claude 里调用 aca limits --mod(无需手动输入)。见 aca help config。

供菜单栏这类程序使用(都不联网):
  --json    当前每个账号的额度,输出一个 JSON:{\"now\", \"accounts\": [{\"tool\", \"name\", \"dir\", \"wins\": [{\"label\", \"used\", \"resets\", \"at\"}]}]}。
            Claude 的是记录下来并合并后的(与计费页相同),Codex 的读取它最近的对话记录;name 是登记的账号名,未登记的是去掉点的目录名
数字有变化时还会往事件流写一条 limits.changed(字段与上面单个账号的相同),订阅它就不用定期查询:aca events -f,
或者在插件清单里写 on = \"limits.changed\"。Codex 的在它一轮结束时检查。",
                    "\
For keeping your own status bar: in Claude's settings.json make the statusLine command \"aca limits -- <yours>\".
The recorded quotas show at the top of the billing panel. To have aca draw the status bar too, use aca statusline.
Per-model weekly quotas (Fable) aren't in the status bar: when an account's numbers move, at most every five minutes
it starts aca limits --ask in the background to ask Claude itself (claude -p's get_usage, the same source as /usage);
only registered accounts are asked.

To record them without the status bar: turn on m in ^S of the interface ([tools.claude] mod = true in config.toml) and aca starts
Claude with its own mod, which calls aca limits --mod inside Claude (you don't type it). See aca help config.

For programs such as a menu bar (no network either way):
  --json    every account's quota now, as one JSON: {\"now\", \"accounts\": [{\"tool\", \"name\", \"dir\", \"wins\": [{\"label\", \"used\", \"resets\", \"at\"}]}]}.
            Claude's are the recorded ones merged (as in the billing panel), Codex's are read from its latest conversation records;
            name is the registered account name, else the directory's name without its dot
When the numbers move, a limits.changed line (fields as one account above) goes into the event stream, so nothing has to keep
asking: aca events -f, or on = \"limits.changed\" in a plugin manifest. Codex's are looked at when one of its turns ends.",
                ),
            },
            Cmd {
                names: &["setup"],
                short: Tr::same("setup [--statusline aca|wrap|off]"),
                brief: Tr::new(
                    "安装后的设置:登记账号、各账号是否接入 Claude 的状态栏(一张表)",
                    "after installing: register accounts, wire Claude status bars",
                ),
                usage: &[Tr::same("setup [--statusline aca|wrap|off]")],
                more: Tr::new(
                    "  --statusline  不打开表,所有 Claude 账号都按后面指定的方式接入:
  aca           状态栏由 aca statusline 绘制
  wrap          保留你自己的状态栏,外面套一层 aca limits,只记录额度(没有状态栏的账号不变)
  off           撤销 aca 的接入,恢复到接入前的样子

不带参数时打开一张表:每行一个账号(已登记的,以及未登记的 ~/.claude),Claude 账号的右边是状态栏的接入方式。
  ↑↓ 选择账号 · ←→ 切换接入方式(aca 的 / 自己的 + 额度 / 不接入)· a 全部应用此项 · + 添加账号(名字、配置目录)· x 取消添加
  ⏎ 确定:先登记账号,再修改状态栏(只改选择与当前不同的),并打印做了什么、新目录如何登录;Esc 取消,不做任何更改
不在终端里运行时只列出。不打开表登记账号:aca account add。界面里设置页(^S)的账号页按 s 是同一张表。

Claude 只通过状态栏的输入提供订阅额度,所以要在它每个账号的 settings.json 里把 statusLine 指向 aca。
只修改 statusLine 这一项,文件其他内容一个字都不改;你原来的设置保存在 <state>/setup/statusline.json,不接入 / off 时按它恢复。
已打开的 Claude 会话会立即生效,无需重启。",
                    "  --statusline  no table; wire every Claude account the way that follows:
  aca           the status bar is drawn by aca statusline
  wrap          keep your own status bar, wrapped in aca limits just to record quotas (accounts without one: untouched)
  off           undo what aca wired, back to how it was before

Without arguments it's a table: one row per account (the registered ones, and an unregistered ~/.claude); Claude
accounts show how their status bar is wired on the right.
  ↑↓ pick an account · ←→ change wiring (aca draws / yours + quotas / none) · a: all like this row · + add an account · x: don't add it
  ⏎ do it: register first, then change status bars (only rows changed), print what was done and how to log in; Esc: nothing
Not in a terminal it only lists. Registering without the table: aca account add. In the interface's settings page (^S),
s on the accounts opens the same table.

Claude only hands out subscription quotas in the status bar's input, so each account's settings.json needs its statusLine
pointed at aca. Only the statusLine entry changes, nothing else in the file; your original is kept in
<state>/setup/statusline.json, and none / off restores it.
Open Claude sessions switch over immediately, no restart needed.",
                ),
            },
            Cmd {
                names: &["statusline"],
                short: Tr::same("statusline"),
                brief: Tr::new(
                    "Claude 的状态栏:上下文、模型、分支和改动、目录,以及账号额度",
                    "Claude's status bar: context, model, branch, dir, plus quotas",
                ),
                usage: &[Tr::same("statusline")],
                more: Tr::new(
                    "\
在 Claude 的 settings.json 里把 statusLine 的命令写成「aca statusline」(想让空闲的会话也随之更新,加上 \"refreshInterval\": 秒数)。
空闲(对话记录 30 秒没有变化)且没有人连接查看的会话,按上次绘制的内容原样输出:否则它会跟着其他会话一起重绘,屏幕一直有输出,自动停止就永远等不到它空闲。
连接进去查看时,下一次刷新就是最新的。
两行:上下文 │ 模型 │ 分支和改动 │ 目录;额度 │ 花费和时长 │ Python │ node。没有的部分不显示。
额度像 aca limits 一样记录,不需要再套一层;显示这个账号所有会话里最新的一份,用量超过八成时附带距离重置还有多久。
分支每次直接读取仓库的 HEAD;改动的文件数、行数、领先落后按目录缓存 5 秒。配色是 Catppuccin Latte,图标需要 Nerd Font。",
                    "\
In Claude's settings.json make the statusLine command \"aca statusline\" (to keep idle sessions updating too, add
\"refreshInterval\": seconds).
A session that is idle (no transcript write for 30 seconds) and that nobody is attached to prints its last bar as it was:
otherwise it redraws along with the other sessions, its screen never goes quiet, and idle stop never sees it idle.
Attach to it and the next refresh is current.
Two lines: context │ model │ branch and changes │ directory; quota │ cost and duration │ Python │ node. Parts that
don't apply aren't drawn.
Quotas are recorded the way aca limits does, no extra wrapping; it shows the newest across all of this account's
sessions, and past 80% used, how long until the reset.
The branch is read straight from the repository's HEAD every time; changed files, lines, ahead / behind are cached per
directory for 5 seconds. Colors are Catppuccin Latte; the icons need a Nerd Font.",
                ),
            },
            Cmd {
                names: &["tmux", "tmux-status"],
                short: Tr::same("tmux install | uninstall | key"),
                brief: Tr::new(
                    "切换器的按键、状态栏的待处理数:安装 / 卸载;修改「下一个」的键",
                    "switcher keys + status segment: add / remove; the next key",
                ),
                usage: &[
                    Tr::same("tmux [install | uninstall]"),
                    Tr::same("tmux key"),
                    Tr::new("tmux key next <键>", "tmux key next <KEY>"),
                    Tr::same("tmux key next --none"),
                    Tr::same("tmux-status [id]"),
                ],
                more: Tr::new(
                    "\
  不带参数     查看是否已安装;你的 tmux.conf 里是否还有自己手写的 aca 配置行
  install      写入 ~/.config/aca/tmux.conf(prefix + a 全屏打开切换器,不按前缀按 Ctrl+\\ 在右侧打开,状态栏接入 tmux-status),
               并在你的 tmux.conf 里加两行:一行注释、一行 source-file -q 它(放在 TPM 那一段之前,没有就放在最后)。已打开的 tmux 立即生效
  uninstall    去掉那两行、删除 aca 的那份配置;已打开的 tmux 里 aca 绑定的键和状态栏那一段也一并撤销
  key          aca 自己绑定在 tmux 上、你可以修改的键:当前是哪个、是否已绑定。它们不在上面那份配置里:和 Ctrl+Q 一样
               由 aca 直接绑定,没有 install 也存在。目前只有一个:
  next         进入下一个待处理的会话(aca help next),默认 Ctrl+Alt+N。后面给一个键就改成它(tmux 的键名:M-n、C-M-n、F7),
               记在 config.toml 的 [keys] next,并立即绑定
  --none       不绑定这个键(仍然可以输入 aca next)
  tmux-status  状态栏上的那一段,显示为「 ◆ 2  ◐ 3」:只有类别和个数,符号与列表相同。◆ 等待回应(派出去的 worker 也算):
               agent 停下来等你,红底;◐ 未读:已完成但尚未查看(只算你自己开的),次要文字色。不显示名字:是谁、在问什么看列表,
               按 Ctrl+Alt+N(aca next)直接过去。后面的 ⚠ bark 是出错的插件(钩子、定时上次没有成功运行,gateway 的服务停止了;
               具体是哪一处见 aca plugin ls)。id 是你正在看的会话(#{@aca_id}),它不计入;都没有时什么也不显示。
               tmux 每隔 status-interval 秒运行一次,所以只读状态文件和一次 tmux list-panes,不读屏幕:偶尔会比列表慢一拍

aca 的那份配置由 aca 管理:每次 install 都会重写,升级后打开界面(包括 ^L 重启)时也会重写,./install.sh 升级时会立即重写。
不要在里面修改;要改键,在你的 tmux.conf 里 source 它的那一行之后自己 unbind / bind。
你的 tmux.conf 只会加上、删除那两行,其他内容不变;如果它是链接,写入它指向的文件。
-q:aca 删除、那份配置不存在后,tmux 也不会报错。不想安装、自己接入状态栏:set -ag status-right \"#(aca tmux-status #{@aca_id})\"",
                    "\
  (none)       installed or not; whether your tmux.conf still has aca lines you wrote yourself
  install      writes ~/.config/aca/tmux.conf (prefix + a opens the switcher full screen, Ctrl+\\ without the prefix opens it
               on the right, the status bar gets tmux-status) and adds two lines to your tmux.conf: a comment and
               source-file -q of it (before the TPM part, or at the end). The running tmux picks it up at once
  uninstall    removes those two lines and aca's file; aca's keys and status segment leave the running tmux too
  key          the keys aca binds in tmux itself that you can change: which they are, bound or not. They aren't in the
               file above: aca binds them directly, the way Ctrl+Q is, installed or not. One for now:
  next         go to the next session waiting for you (aca help next), Ctrl+Alt+N by default. Give a key to change it
               (a tmux key name: M-n, C-M-n, F7); kept as [keys] next in config.toml and bound at once
  --none       no key for it (aca next still works typed)
  tmux-status  the status-bar segment, like \" ◆ 2  ◐ 3\": kinds and counts only, same symbols as the list. ◆ needs you
               (dispatched workers count too): an agent is stopped until you answer, so it's a red block;
               ◐ unread: finished and not looked at (only the ones you started), in a minor color. No names: who and what they
               ask is in the list, and Ctrl+Alt+N (aca next) takes you there. Then ⚠ bark names plugins that are failing
               (a hook or timer failed last time, a gateway server stopped; aca plugin ls says where). The id is the
               session you're looking at (#{@aca_id}), which doesn't count; nothing to show prints nothing.
               tmux runs it every status-interval seconds, so it only reads state files and one tmux list-panes, not
               screens: now and then it's a beat behind the list

aca's file belongs to aca: every install rewrites it, so does opening the interface (or Ctrl+L) after an upgrade, and
./install.sh rewrites it as it upgrades. Don't edit it; to change a key, unbind / bind after the line in your tmux.conf
that sources it. Your tmux.conf only gains / loses those two lines, nothing else changes; a symlink is written
through to its target.
-q: with aca removed and its file gone, tmux doesn't complain. Without installing, the status segment by hand:
set -ag status-right \"#(aca tmux-status #{@aca_id})\"",
                ),
            },
            Cmd {
                names: &["theme"],
                short: Tr::new("theme [名字]", "theme [NAME]"),
                brief: Tr::new("界面配色:查看、切换、新建主题", "interface colors: show, switch, create a theme"),
                usage: &[
                    Tr::new("theme [名字]", "theme [NAME]"),
                    Tr::new("theme use <名字>", "theme use <NAME>"),
                    Tr::new("theme new <名字> [从哪套]", "theme new <NAME> [FROM]"),
                ],
                more: Tr::new(
                    "\
不带参数:显示当前主题的每种颜色(名字、色块、用途)和所有主题;带名字时显示那个主题。
  use    切换到那个主题(记在 config.toml 的 [theme])
  new    在 ~/.config/aca/themes/ 生成一份主题文件(全部注释掉,去掉 # 再修改)

环境变量 ACA_THEME=主题名 优先于配置(dark / light 即 mocha / latte)。",
                    "\
Without arguments: every color of the current theme (name, swatch, where it's used) and all themes; with a name, that one.
  use    switch to it (recorded under [theme] in config.toml)
  new    write a theme file into ~/.config/aca/themes/ (all commented out: remove a # to change it)

The environment variable ACA_THEME=<name> overrides the config (dark / light are mocha / latte).",
                ),
            },
        ],
    ),
    (
        Tr::new("供插件和脚本使用", "For plugins and scripts"),
        &[
            Cmd {
                names: &["events"],
                short: Tr::same("events [-f]"),
                brief: Tr::new("事件流:会话上发生的每件事,一行一个 JSON", "event stream: everything that happens to sessions, JSON lines"),
                usage: &[Tr::new("events [-f] [-n 行数]", "events [-f] [-n LINES]")],
                more: Tr::new(
                    "  -f         持续读取新事件
  -n 行数    先打印最近几行(默认 20;带 -f 时默认不打印)

有哪些事件、各自带哪些字段:https://github.com/cheny-00/aca/blob/main/docs/plugins.zh-CN.md",
                    "  -f          keep following
  -n LINES    print the last few lines first (default 20; with -f, none by default)

Which events there are and their fields: https://github.com/cheny-00/aca/blob/main/docs/plugins.md",
                ),
            },
            Cmd {
                names: &["meta"],
                short: Tr::same("meta set | rm"),
                brief: Tr::new("显示在会话行上的短标签(PR 号、CI 状态 …)", "a short tag on a session's row (PR number, CI status …)"),
                usage: &[
                    Tr::new("meta set <id> <名字> <值> [--ttl 秒]", "meta set <id> <NAME> <VALUE> [--ttl SECS]"),
                    Tr::new("meta rm <id> <名字>", "meta rm <id> <NAME>"),
                ],
                more: Tr::new(
                    "  set      添加标签(替换同名的)。名字:1–32 个字母、数字、_ -;值最长 80 个字,超出截断。
           行上显示为 [名字 值];名字以 _ 开头的只显示值
  rm       移除标签
  --ttl    这么多秒后不再显示;不写时一直显示",
                    "  set      attach it (replacing one with the same name). NAME: 1–32 letters, digits, _ -; VALUE: cut at 80 characters.
           Shown as [NAME VALUE]; a NAME starting with _ shows only the value
  rm       remove it
  --ttl    stop showing it after this many seconds; default: stays",
                ),
            },
            Cmd {
                names: &["plugin"],
                short: Tr::same("plugin install | ls | run | …"),
                brief: Tr::new(
                    "插件:安装、删除、开关、调用能力、打开窗格(界面里按 P 管理)",
                    "plugins: install, remove, switch, actions, panes (or press P)",
                ),
                usage: &[
                    Tr::new(
                        "plugin install <git 地址 | owner/repo | 本地目录 | aca.名字> [--ref 版本]",
                        "plugin install <GIT-URL | owner/repo | LOCAL-DIR | aca.NAME> [--ref REV]",
                    ),
                    Tr::new("plugin update [插件 id]", "plugin update [PLUGIN-ID]"),
                    Tr::same("plugin sync"),
                    Tr::same("plugin gateway [status | stop]"),
                    Tr::new("plugin gateway restart [服务]", "plugin gateway restart [SERVER]"),
                    Tr::new("plugin gateway log <服务>", "plugin gateway log <SERVER>"),
                    Tr::new("plugin gateway add | drop <名字>", "plugin gateway add | drop <NAME>"),
                    Tr::new("plugin gateway import [--apply] [名字…]", "plugin gateway import [--apply] [NAME…]"),
                    Tr::new("plugin gateway off | on <服务> [--session id]", "plugin gateway off | on <SERVER> [--session id]"),
                    Tr::new("plugin remove | enable | disable | log <插件 id>", "plugin remove | enable | disable | log <PLUGIN-ID>"),
                    Tr::new("plugin new <目录> [--id 插件 id]", "plugin new <DIR> [--id PLUGIN-ID]"),
                    Tr::new("plugin check [目录 | 插件 id]", "plugin check [DIR | PLUGIN-ID]"),
                    Tr::same("plugin timer [--json]"),
                    Tr::new("plugin timer on | off | run <插件.能力>", "plugin timer on | off | run <PLUGIN.COMMAND>"),
                    Tr::new(
                        "plugin emit <事件> [字段=值…] [--session id] [--plugin 插件 id]",
                        "plugin emit <EVENT> [FIELD=VALUE…] [--session id] [--plugin PLUGIN-ID]",
                    ),
                    Tr::new("plugin link <目录>", "plugin link <DIR>"),
                    Tr::new("plugin unlink <插件 id>", "plugin unlink <PLUGIN-ID>"),
                    Tr::same("plugin ls"),
                    Tr::same("plugin actions [--json]"),
                    Tr::new("plugin run <插件.能力> [名字=值…] [--session id]", "plugin run <PLUGIN.ACTION> [NAME=VALUE…] [--session id]"),
                    Tr::new("plugin view <插件.视图> [--session id]", "plugin view <PLUGIN.VIEW> [--session id]"),
                    Tr::new("plugin setting <插件 id> [名字] [值]", "plugin setting <PLUGIN-ID> [NAME] [VALUE]"),
                    Tr::new("plugin setting <插件 id> <名字> --reset", "plugin setting <PLUGIN-ID> <NAME> --reset"),
                    Tr::new("plugin allow | deny <插件 id> [能力…]", "plugin allow | deny <PLUGIN-ID> [COMMAND…]"),
                    Tr::new("plugin allow <插件 id> --reset", "plugin allow <PLUGIN-ID> --reset"),
                    Tr::same("plugin key"),
                    Tr::new("plugin key <插件 id> <目标> <键>", "plugin key <PLUGIN-ID> <TARGET> <KEY>"),
                    Tr::new("plugin key <插件 id> <目标> --none", "plugin key <PLUGIN-ID> <TARGET> --none"),
                    Tr::new("plugin key <插件 id> <目标> --reset", "plugin key <PLUGIN-ID> <TARGET> --reset"),
                    Tr::same("plugin hotkey"),
                    Tr::new("plugin hotkey <插件 id> <窗格> <键>", "plugin hotkey <PLUGIN-ID> <PANE> <KEY>"),
                    Tr::new("plugin hotkey <插件 id> <窗格> --none", "plugin hotkey <PLUGIN-ID> <PANE> --none"),
                    Tr::new("plugin hotkey <插件 id> <窗格> --reset", "plugin hotkey <PLUGIN-ID> <PANE> --reset"),
                    Tr::new(
                        "plugin pane <插件> <窗格> [--session id] [--placement popup|split|window] [--client 客户端]",
                        "plugin pane <PLUGIN> <PANE> [--session id] [--placement popup|split|window] [--client CLIENT]",
                    ),
                ],
                more: Tr::new(
                    "\
插件 = 一个目录 + 一份 aca-plugin.toml(能力、事件钩子、定时、窗格、设置、带给 agent 的目录和 MCP),再通过 aca 的命令行回调。
写法:https://github.com/cheny-00/aca/blob/main/docs/plugins.zh-CN.md
用哪些插件、给哪些 agent、各项设置写在 ~/.config/aca/plugins.toml;下面这些命令也会修改它。
不想单独建插件:写好 ~/.config/aca/user/aca-plugin.toml 就生效(id 固定为 user,无需 link),删除就失效。

  install        git 地址的克隆到 ~/.local/share/aca/plugins/<id>;owner/repo 指 GitHub 上的仓库;本地目录等同于 link;
                 aca.名字 是 aca 自带的插件(aca.delegate、aca.core、aca.gateway、aca.remote、aca.tailnet、aca.bark、
                 aca.discord、aca.notify、aca.copy;列在界面 P 页的「可安装」里),随 aca 一起升级。
                 安装后列出它提供的内容:给 agent 的、插件菜单里的(会话列表上按 :)、订阅的事件、设置
  --ref          固定在某个标签 / 分支 / 提交上(update 不会更新它)
  update         更新 git 安装的插件(只快进);不给 id 时更新全部。新的提交记入 plugins.lock
  sync           让本机与 plugins.toml 一致:安装缺少的(git 的按 plugins.lock 里的提交)、删除没写的、按它设置开关
  gateway        MCP gateway(插件 aca.gateway 开启时):每个 MCP 只启动一份,agent 只挂载 gateway。不写时等于 status
  status         是否在运行、谁已连接、下面每个服务的状态、最近的调用
  stop           停止它(下一个 agent 连接时再启动)
  restart        重启下面的一个服务(不写名字时重启全部),下次有人使用时再启动
  log            一个服务的日志(它的 stderr)
  add            把 config.toml 的 [mcps.<名字>] 加入 gateway(修改 plugins.toml 里 gateway 的 servers 设置)
  drop           移出 gateway
  import         列出各账号里单独配置的 MCP(Claude 的 .claude.json、Codex 的 config.toml);
  --apply        导入:写入 aca 的 config.toml [mcps.*] 再加入 gateway。账号里的配置保持原样,是否删除由你决定
  off            只对一个会话关闭 gateway 下的某个服务(Claude 立即生效,Codex 重启后生效)
  on             重新开启
  remove         不再使用它:git 安装的连同目录删除,链接的目录不变,解除它的热键;插件自己的配置和状态保留(空目录不保留)
  enable         开启
  disable        暂时关闭(不运行它的钩子和能力,不把它的目录带给 agent),enable 重新开启;agent 下次启动时生效
  log            查看它的日志(事件钩子、能力的输出)
  new            新建插件:一份清单(一条命令、一个键、一个设置,另有写好但注释掉的事件钩子)和它运行的脚本,link 后就能试用
  --id           这个插件的 id;默认为 me.<目录名>
  check          检查一个插件(目录或已链接的 id;默认为当前目录),不需要先链接:清单是否正确、命令要运行的程序和脚本
                 是否存在、needs 里的程序本机是否已安装;有问题时退出码为 1
  emit           模拟一个事件:订阅它的钩子在前台运行完,输出直接打印(平时不等钩子运行完,输出写入日志)。
                 测试钩子时不用真的让 agent 运行一轮;字段=值 是事件自己的字段(kind=transient)
  字段=值        事件的字段:数字、true / false 按类型解析,其他当作文本
  --plugin       只运行这个插件的钩子
  timer          定时任务(清单的 [[timers]],到点由 launchd 唤起运行):不写时列出每个是否开启、间隔多久、上次运行结果
                 (--json 每行一个)。on / off 开启或关闭(manual 的平时由插件自己开关);run 立即运行一次,和到点时一样
                 (打印输出,关闭时也会运行)
  link           链接一个插件目录(读取它的 aca-plugin.toml)
  unlink         取消链接(目录不变)
  ls             插件:● 已开启 ○ 已关闭、名字和版本、来源、给哪种 agent 提供内容
  actions        已开启插件的能力、窗格和视图
  --json         每行一个能力的 JSON(供脚本读取)
  run            调用一个能力
  名字=值        能力的参数(清单 args 里写的),按其类型检查
  view           运行一个视图,打印它的输出(界面里是放进全屏阅读页)
  setting        插件的设置:不给名字时列出全部;给名字时打印它的当前值(供脚本、agent 读取);再给值就修改(写入 plugins.toml)
  --reset        恢复为清单里的默认值
  allow          把这几个能力作为工具提供给 agent(写入 plugins.toml 的 tools);不写能力时列出可提供的(✓ 表示已提供)。
                 清单里写 agent? 的(会改动你机器上内容的)默认不提供,需在这里明确允许。已打开的会话重启后生效
  deny           不再提供
  key            : 菜单里的键:不给插件时列出全部(没分到键的会标出);给了就把这一项(能力 / 窗格 / 视图)改成这个键
                 (写入 plugins.toml 的 keys)。两个插件要用同一个键时,你指定的优先
  --none         不绑定键(在 : 菜单里选)
  hotkey         会话里的热键(不用前缀,像 Ctrl+Q;往上翻看历史时也能按):按下打开插件的某个窗格。aca 把它绑定在 tmux 上,
                 只在 aca 的会话里拦截,在其他会话里按键照常发给里面的程序;插件关闭或删除后自动解除,不用改 tmux.conf。
                 不给插件时列出全部(没绑定上的会标出);给了就把这个窗格改成这个键(tmux 的键名:C-]、M-y、C-M-y、F5;
                 写入 plugins.toml 的 hotkeys)。清单没给热键的窗格也能设一个;你自己在 tmux 里绑定了其他用途的键不会被覆盖
  pane           打开它的一个窗格
  --session      针对哪个会话;默认为当前所在的会话(agent 环境里的 ACA_SESSION_ID;在它的 tmux 会话里另开的窗口也算)
  --placement    窗格在哪里打开;不写时按插件自己的设置,都没写就是 popup
  popup          tmux 弹窗(不在 tmux 里时在当前终端前台运行)
  split          在 agent 那一格的右侧打开
  window         那个 tmux 会话里的新窗口
  --client       弹窗显示在哪个 tmux 客户端上",
                    "\
A plugin = a directory + an aca-plugin.toml (commands, event hooks, timers, panes, settings, a directory and an MCP server
it brings to agents) that calls back into aca's command line.
How to write one: https://github.com/cheny-00/aca/blob/main/docs/plugins.md
Which plugins, for which agents, and their settings live in ~/.config/aca/plugins.toml; these commands edit it too.
No plugin to make: write ~/.config/aca/user/aca-plugin.toml and it's there (its id is always user, no link); delete it and it's gone.

  install        git ones are cloned to ~/.local/share/aca/plugins/<id>; owner/repo is on GitHub; a local directory is a
                 link; aca.NAME is one that ships with aca (aca.delegate, aca.core, aca.gateway, aca.remote,
                 aca.tailnet, aca.bark, aca.discord, aca.notify, aca.copy; listed under “Available” on the P page) and upgrades with it. Afterwards it lists what the plugin brings: for agents, the plugin menu (press :
                 on the session list), events it subscribes to, settings
  --ref          pin to a tag / branch / commit (update leaves it alone)
  update         pull git ones (fast-forward only); no id = all of them. The new commit goes into plugins.lock
  sync           make this machine match plugins.toml: install what's missing (git ones at the commit in
                 plugins.lock), remove what isn't listed, switch on / off as it says
  gateway        the MCP gateway (when the aca.gateway plugin is on): each MCP runs once, agents mount only it.
                 Default: status
  status         running or not, who's connected, each server under it, recent calls
  stop           stop it (it starts again when the next agent connects)
  restart        restart one server under it (no name = all); it starts again when needed
  log            one server's log (its stderr)
  add            serve [mcps.<name>] from config.toml through the gateway (edits its servers setting in plugins.toml)
  drop           take it out of the gateway
  import         list the MCPs scattered in your accounts (Claude's .claude.json, Codex's config.toml);
  --apply        take them in: written to [mcps.*] in aca's config.toml, then served. The accounts are left as they are
  off            switch a server under the gateway off for one session (Claude picks it up now, Codex after a restart)
  on             switch it back on
  remove         stop using it: a git one's directory is deleted, a linked one is left alone, its hotkeys are unbound;
                 its own config and state stay (empty directories don't)
  enable         switch it on
  disable        pause it (no hooks, no actions, its directories aren't brought to agents); enable again to resume;
                 agents change on their next start
  log            its log (output of event hooks and actions)
  new            create a plugin: a manifest (a command, a key, a setting, an event hook written out but commented) and the
                 script it runs; link it and try it
  --id           the plugin id; default me.<directory name>
  check          check a plugin (a directory or a linked id; default the current directory) without linking it: is the
                 manifest right, are the programs and scripts its commands run there, are the programs in needs on this
                 machine; exits 1 on a problem
  emit           pretend an event happened: the hooks subscribed to it run in the foreground and their output is printed
                 (normally hooks aren't waited for and log their output). Test hooks without running an agent;
                 FIELD=VALUE are the event's own fields (kind=transient)
  FIELD=VALUE    the event's fields: numbers and true / false keep their type, the rest is text
  --plugin       only this plugin's hooks
  timer          timers ([[timers]] in manifests; launchd runs them when due): with nothing, lists each one: on or off,
                 how often, how the last run went (--json one per line). on / off switch one (manual ones are normally
                 switched by their plugin); run runs one now as if it were due (output printed, even when off)
  link           link a plugin directory (reads its aca-plugin.toml)
  unlink         unlink it (the directory is left alone)
  ls             plugins: ● on ○ off, name and version, where from, which agents it brings things to
  actions        actions, panes and views of the plugins that are on
  --json         one JSON action per line (for scripts)
  run            run an action
  NAME=VALUE     the action's arguments (the args in its manifest), checked against the types written there
  view           run a view and print its output (the interface puts it in the full-screen reader)
  setting        a plugin's settings: no name lists them all; a name prints its value (for scripts and agents);
                 a value too changes it (written to plugins.toml)
  --reset        back to the manifest's default
  allow          give agents these commands as tools (written to tools in plugins.toml); no command = list what it can
                 give (✓ given). Commands marked agent? in the manifest (ones that change things on your machine) aren't
                 given until you allow them here. Running sessions change after a restart
  deny           stop giving them
  key            keys in the : menu: no plugin lists them all (naming any that lost a key); otherwise gives this one
                 (command / pane / view) that key (written to keys in plugins.toml). When two plugins want one key, yours wins
  --none         no key (pick it in the : menu)
  hotkey         hotkeys inside a session (no prefix, like Ctrl+Q; they work while scrolled back too): pressing one opens a
                 pane of the plugin. aca binds it in tmux, only for aca's sessions; elsewhere the key still reaches the
                 program. Disable or remove the plugin and it is unbound; tmux.conf isn't touched. No plugin lists them
                 all (naming any that couldn't be bound); otherwise gives this pane that key (a tmux key name: C-], M-y,
                 C-M-y, F5; written to hotkeys in plugins.toml). A pane the manifest gave no hotkey can get one too; a key
                 you bound to something else in tmux is left alone
  pane           open one of its panes
  --session      which session it's for; default: the one you're in (ACA_SESSION_ID in an agent's environment, or the
                 session whose tmux you opened this window in)
  --placement    where the pane opens; default: what the plugin says, and popup if it says nothing
  popup          a tmux popup (outside tmux, runs in the foreground of this terminal)
  split          to the right of the agent's pane
  window         a new window in that tmux session
  --client       which tmux client the popup shows up on",
                ),
            },
            Cmd {
                names: &["hook"],
                short: Tr::new("hook <工具>", "hook <TOOL>"),
                brief: Tr::new("由 agent 的 hook 调用,不供手动使用", "called by agent hooks, not meant for people"),
                usage: &[Tr::new("hook <claude | codex | 工具名>", "hook <claude | codex | TOOL-NAME>")],
                more: Tr::new(
                    "  claude    Claude Code 的 hook
  codex     Codex 的 hook
  工具名    自己登记的声明式工具的 hook

每个事件调用一次,事件的 JSON 在标准输入里;属于哪个会话由环境变量 ACA_SESSION_ID 决定。",
                    "  claude       Claude Code's hook
  codex        Codex's hook
  TOOL-NAME    the hook of a declared tool you registered

Called once per event, with the event's JSON on standard input; the session comes from ACA_SESSION_ID in the
environment.",
                ),
            },
        ],
    ),
];

/// The pages that aren't commands
pub const TOPICS: &[(&str, Tr)] =
    &[("keys", Tr::new("界面快捷键", "interface keys")), ("config", Tr::new("config.toml 的写法", "writing config.toml"))];

const CONFIG: &str = "\
~/.config/aca/config.toml —— 全部可选,一个字不写也能用(claude / codex 使用各自默认的配置目录)。
修改后立即生效,不需要重启;格式有误时命令行会提示一次,并当作没写。

账号 = 启动 agent 时额外带上的内容,用工具自己的写法写在一处(aca new 工具:账号)
  [accounts.claude]
  work = \"~/.claude-work\"                                     # 只换配置目录:写一个字符串
  bedrock = { env = { CLAUDE_CODE_USE_BEDROCK = \"1\" } }        # 不换目录,换认证方式
  [accounts.codex]
  team = { home = \"~/.codex2\", args = [\"--profile\", \"team\"] }  # 除了目录还要带参数
  api = { env_file = \"~/.secrets/openai.env\" }                # key 放在文件里,每次启动时读取(aca 不保存)
  未登记的目录不必写在这里:aca new codex:~/.codex-try

工具
  [tools.claude]                   # 内置工具(claude / codex / pi / opencode / shell)的默认值
  account = \"work\"                 # 不指定账号时用哪个
  args = [\"--dangerously-skip-permissions\"]                   # 每次启动都带的参数
  env = { ANTHROPIC_BASE_URL = \"https://…\" }
  mod = true                       # 启动 Claude 时带上 aca 的 mod:不靠状态栏获取订阅额度(不写:不带;也可在界面 ^S 里按 m 开关)
  [tools.codex]
  cmd = [\"~/bin/codex-wrapper.sh\"] # 自己套了一层脚本时
  hook_trust = \"bypass\"            # 不让 Codex 询问「Hooks need review」(不写:每个账号询问一次)
  [tools.ccx]                      # 自己命名的工具:aca new ccx
  kind = \"claude\"                  # 按哪种内置工具启动、接入 hook
  args = [\"--model\", \"opus\"]
  [tools.aider]                    # 不写 kind:声明式工具,其他 agent 不用写代码就能接入
  resume = [\"--session\", \"{conv}\"]
  busy = ['Working\\.\\.\\.']         # 屏幕上出现就算运行中(正则);ask 同理

会话之间传话(aca help msg)
  [inbox]
  notify = [\"done\", \"failed\"]     # worker 在哪些事件上通知派它出来的会话(all = 全部;不写 = 都不通知)

启动时的确认框
  [trust]
  folder = true                    # 替你回答「是否信任这个目录」(Claude、Codex 第一次进入一个目录时都会问;不写:不替你回答,
                                   # 会话停在那里等你;也可在界面 ^S 里按 f 开关)。之后那个目录自带的配置无需询问就能运行

空闲的会话(aca help stop;有误时只忽略这一节)
  [idle]
  stop_after = \"3h\"                # 空闲这么久就停止,列表里显示 ☾(30m、3h、1d、1h30m;never 或不写 = 不自动停止)。aca 界面打开时每 5 分钟检查一次;
                                   # 本轮未结束的、未读的、等待回应的、有人连着的、有后台任务的、输入框里有未发送文字的、
                                   # 派出去的会话仍未停止的、自己设了定时且还没到点的都不停止(aca help stop)。⏎ 启动并继续原来的对话
  remove_after = \"30d\"             # 已停止(■ ☾,包括已归档的)这么久没有动静就删除,和 d 一样移到回收站,1 天内可用 ^Z / b 恢复
                                   # (never 或不写 = 不删除)。置顶的、有便签的、派出去的会话仍未停止的不删除。Claude 自己的对话记录
                                   # 默认保留 30 天,停止超过这么久的,重新启动也无法恢复原来的对话

aca 绑定在 tmux 上的键(不用前缀,只在 aca 的会话里拦截;不写入你的 tmux.conf)
  [keys]
  next = \"C-M-n\"                   # 进入下一个待处理的会话(aca help next)。不写时就是它;\"\" = 不绑定;aca tmux key next <键> 也是修改这里

登录(账号页的 ⏎、aca run … login:不替你打开默认浏览器,弹出链接供选择;有误时只忽略这一节)
  [login]
  browsers = [\"Firefox\", { name = \"Chrome 工作\", cmd = [\"google-chrome\", \"--profile-directory=Profile 1\"] }]
                                   # 不写:自动查找(Mac 上找 .app,Linux 上找 .desktop);写了就只列出这些。
                                   # 字符串:Mac 上是 App 的名字,Linux 上是命令;表:名字 + 命令,链接附在最后(写 {url} 就放在那里)
  remote = \"auto\"                  # 远程(不列出浏览器,改用能在其他设备上完成的登录):auto 按是否 SSH、Linux 是否有图形界面判断;yes / no 固定

界面
  [ui]
  lang = \"zh\"                      # 界面语言:en(默认)/ zh;环境变量 ACA_LANG 优先
  menu = [\"work\", \"team\"]          # n 的工具菜单里哪些排在前面
  hide = [\"official\"]              # 隐藏不常用的
  [mcps.web]                       # MCP 目录:按 M 可挂载到项目(除了 description、scope,其他键原样写入 .mcp.json);
  command = \"npx\"                  # gateway 的 servers 设置里指定的,经 gateway 提供给所有 agent(aca help plugin)
  args = [\"-y\", \"web-mcp\"]
  description = \"搜网页\"
  scope = \"session\"                # 在 gateway 里每个会话一份(有状态的,比如浏览器);不写 = 所有会话共用一份
  [theme] name · [ime] auto · [new_group] default_path · [hooks] url / token
                                   # 这几项在界面的 ^S 设置页里修改,不用手写

当前可用的名字:aca names。账号的每个选项、防止账号混用的规则:https://github.com/cheny-00/aca/blob/main/docs/accounts.zh-CN.md";

const CONFIG_EN: &str = "\
~/.config/aca/config.toml — all optional; it works with nothing in it (claude / codex use their default config dirs).
Changes apply at once, nothing to restart; if it's malformed the command line says so once and treats it as empty.

Accounts = what to bring along when starting an agent, written once in its own terms (aca new TOOL:ACCOUNT)
  [accounts.claude]
  work = \"~/.claude-work\"                                     # only a different config directory: a string
  bedrock = { env = { CLAUDE_CODE_USE_BEDROCK = \"1\" } }        # same directory, different auth
  [accounts.codex]
  team = { home = \"~/.codex2\", args = [\"--profile\", \"team\"] }  # a directory plus arguments
  api = { env_file = \"~/.secrets/openai.env\" }                # the key lives in a file, read at each start (aca keeps none)
  Unregistered directories don't need to be here: aca new codex:~/.codex-try

Tools
  [tools.claude]                   # defaults for the built-ins (claude / codex / pi / opencode / shell)
  account = \"work\"                 # which account when none is given
  args = [\"--dangerously-skip-permissions\"]                   # arguments for every start
  env = { ANTHROPIC_BASE_URL = \"https://…\" }
  mod = true                       # start Claude with aca's mod: quotas without the status bar (unset: none; m in ^S toggles)
  [tools.codex]
  cmd = [\"~/bin/codex-wrapper.sh\"] # wrapped in a script of your own
  hook_trust = \"bypass\"            # keep Codex from asking \"Hooks need review\" (default: asks once per account)
  [tools.ccx]                      # a tool you name yourself: aca new ccx
  kind = \"claude\"                  # which built-in it starts as and hooks into
  args = [\"--model\", \"opus\"]
  [tools.aider]                    # no kind: a declared tool, any agent wired up without code
  resume = [\"--session\", \"{conv}\"]
  busy = ['Working\\.\\.\\.']         # on screen = running (a regex); ask works the same way

Messages between sessions (aca help msg)
  [inbox]
  notify = [\"done\", \"failed\"]     # what a worker tells the one that dispatched it about (all = everything; unset = nothing)

Startup gates
  [trust]
  folder = true                    # answer \"trust this folder?\" for you (Claude and Codex ask the first time they enter
                                   # a folder; unset: not answered, the session waits for you; f in ^S toggles it).
                                   # The folder's own config then runs without asking

Idle sessions (aca help stop; a mistake drops only this)
  [idle]
  stop_after = \"3h\"                # stop sessions idle this long, drawn ☾ in the list (30m, 3h, 1d, 1h30m; never or unset = never).
                                   # Checked every 5 minutes while the aca interface is open; never mid-turn, finished-unread,
                                   # waiting for you, attached, with background tasks or unsent text, with dispatched
                                   # sessions still open, or with a schedule of its own not yet due (aca help stop). ⏎ resumes
  remove_after = \"30d\"             # delete sessions stopped (■ ☾, archived ones too) this long, into the trash like d: ^Z / b
                                   # brings them back within a day (never or unset = never). Not pinned ones, ones with a note,
                                   # or ones with dispatched sessions still open. Claude keeps its own records 30 days by
                                   # default, so one stopped longer can't resume its conversation anyway

Keys aca binds in tmux (no prefix, only inside aca's sessions; nothing written to your tmux.conf)
  [keys]
  next = \"C-M-n\"                   # go to the next session waiting for you (aca help next). Unset: this; \"\" = not bound;
                                   # aca tmux key next <key> edits this

Logins (⏎ on the accounts page, aca run … login: no default browser, the link pops up to pick; a mistake drops only this)
  [login]
  browsers = [\"Firefox\", { name = \"Work Chrome\", cmd = [\"google-chrome\", \"--profile-directory=Profile 1\"] }]
                                   # unset: found by itself (.app on a Mac, .desktop files on Linux); set: only these.
                                   # A string is an app name on a Mac, a command on Linux; a table is a name + a command,
                                   # the link goes last ({url} puts it there instead)
  remote = \"auto\"                  # remote (no browsers, a login you can finish on another device): auto looks at SSH
                                   # and whether Linux has a display; yes / no fixes it

Interface
  [ui]
  lang = \"zh\"                      # language: en (default) / zh; the ACA_LANG environment variable overrides it
  menu = [\"work\", \"team\"]          # who comes first in the n tool menu
  hide = [\"official\"]              # hide the ones you rarely use
  [mcps.web]                       # the MCP catalog: M attaches them to a project (all keys but description and scope
  command = \"npx\"                  # go into .mcp.json); the ones named in the gateway's servers setting go to every
  args = [\"-y\", \"web-mcp\"]       # agent through the gateway (aca help plugin)
  description = \"web search\"
  scope = \"session\"                # one per session in the gateway (stateful ones, a browser); default: one shared
  [theme] name · [ime] auto · [new_group] default_path · [hooks] url / token
                                   # these are set in the interface's ^S settings page, no need to write them

Which names work now: aca names. Every account option and the rules that keep accounts apart: https://github.com/cheny-00/aca/blob/main/docs/accounts.md";

/// How many cells it takes in a terminal: CJK characters and full-width punctuation take two, everything else one (`…` `·`
/// `→` and the like are one in most terminals)
fn width(s: &str) -> usize {
    let wide = |c: char| matches!(c as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6);
    s.chars().map(|c| if wide(c) { 2 } else { 1 }).sum()
}

pub fn find(name: &str) -> Option<&'static Cmd> {
    GROUPS.iter().flat_map(|g| g.1.iter()).find(|c| c.names.contains(&name))
}

/// Which command a mistyped name resembles: it's the start of some name, or only a letter or two off
pub fn guess(name: &str) -> Option<&'static str> {
    let names = || GROUPS.iter().flat_map(|g| g.1.iter()).flat_map(|c| c.names.iter().copied());
    let starts: Vec<&str> = names().filter(|n| !name.is_empty() && n.starts_with(name)).collect();
    if let [one] = starts.as_slice() {
        return Some(one);
    }
    // For short names, two letters off is another word (nope isn't move)
    let far = if name.chars().count() < 5 { 1 } else { 2 };
    names().map(|n| (distance(name, n), n)).filter(|(d, _)| *d <= far && *d < name.chars().count()).min().map(|x| x.1)
}

/// How many letters two words differ by (an insertion, deletion or change counts as one each)
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut diag = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let up = row[j];
            row[j] = (diag + usize::from(a[i - 1] != b[j - 1])).min(up + 1).min(row[j - 1] + 1);
            diag = up;
        }
    }
    row[b.len()]
}

/// Printed before the usage when arguments are wrong; usage forms from the second on are indented to the same width
pub const USAGE: Tr = Tr::new("用法:", "usage: ");

/// The usage of a command, one form per line (also printed when arguments are wrong)
pub fn usage(name: &str) -> Option<String> {
    let c = find(name)?;
    // Several names sharing one entry: print only the lines of this name
    let own: Vec<&Tr> = c.usage.iter().filter(|u| u.split_whitespace().next() == Some(name)).collect();
    let lines: Vec<String> = if own.is_empty() {
        c.usage.iter().map(|u| format!("aca {u}")).collect()
    } else {
        own.iter().map(|u| format!("aca {u}")).collect()
    };
    Some(lines.join(&format!("\n{}", " ".repeat(width(&USAGE)))))
}

/// The overview
pub fn overview() -> String {
    let pad = GROUPS.iter().flat_map(|g| g.1.iter()).map(|c| width(&c.short)).max().unwrap_or(0) + 3;
    let row = |left: &str, right: &str| format!("  {left}{}{right}\n", " ".repeat(pad.saturating_sub(width(left))));
    let mut out = tr!(
        "aca — 在一个屏幕上管理所有 agent 会话(Claude Code / Codex …)。基于 tmux,tmux 会话名即标题\n\n",
        "aca — every agent session (Claude Code / Codex …) on one screen, on tmux; session name = title\n\n"
    );
    out += &row(
        "aca",
        &tr!(
            "打开界面:列表 + 高亮会话的实时画面,回车进入(按 ? 查看快捷键)",
            "the interface: list + live view; Enter goes in, ? shows keys"
        ),
    );
    out += &row(
        &tr!("aca help <命令>", "aca help <COMMAND>"),
        &tr!("一条命令的详细说明(也可以用 aca <命令> --help)", "one command in detail (aca <COMMAND> --help too)"),
    );
    for (title, cmds) in GROUPS {
        out += &format!("\n{title}\n");
        for c in *cmds {
            out += &row(&c.short, &c.brief);
        }
    }
    out += &tr!(
        "\n<id> 可以只写开头几位,也可以写标题。ls / show / new / adopt / group ls 加 --json 供程序读取。\n",
        "\n<id>: first few characters, or the title. --json for programs: ls / show / new / adopt / group ls\n"
    );
    out += &tr!(
        "输出到终端时排版并着色(配色跟随界面主题,设置了 NO_COLOR 时不着色);输出到管道时是不带颜色的原文。\n",
        "In a terminal: laid out, in the interface's colors (NO_COLOR: none); piped: plain.\n"
    );
    let topics: Vec<String> = TOPICS.iter().map(|(n, what)| tr!("aca help {n}({what})", "aca help {n} ({what})")).collect();
    out += &tr!("另见:{} · aca version", "Also: {} · aca version", topics.join(" · "));
    out
}

/// `aca help <NAME>`: the details of a command, or one of those pages; no such name = None
pub fn of(name: &str) -> Option<String> {
    match name {
        "keys" => return Some(aca_tui::KEYS.trim_end().to_string()),
        "config" => return Some(if lang() == Lang::En { CONFIG_EN } else { CONFIG }.to_string()),
        _ => {}
    }
    let c = find(name)?;
    let mut out = String::new();
    for u in c.usage {
        out += &format!("aca {u}\n");
    }
    out += &format!("\n{}", c.brief);
    if !c.more.is_empty() {
        out += &format!("\n\n{}", c.more);
    }
    Some(out)
}

/// Whether the arguments ask for help: `-h` / `--help` before `--`. Not when it follows an argument that takes a message
/// (`-m --help` is the message to send)
pub fn asked(args: &[String]) -> bool {
    let own = args.iter().position(|a| a == "--").map_or(args, |i| &args[..i]);
    own.iter()
        .enumerate()
        .any(|(i, a)| matches!(a.as_str(), "-h" | "--help") && !(i > 0 && matches!(own[i - 1].as_str(), "-m" | "-t" | "--say")))
}

#[cfg(test)]
mod tests {
    use aca_core::i18n::with;

    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_overview_fits_a_screen_and_lists_every_command_once() {
        for l in [Lang::Zh, Lang::En] {
            with(l, || {
                let o = overview();
                assert!(o.lines().count() <= 60, "{l:?}:总览有 {} 行,一屏看不完了", o.lines().count());
                // No wrapping in a 100-column terminal
                assert!(o.lines().all(|l| width(l) <= 100), "{l:?}:{:#?}", o.lines().filter(|l| width(l) > 100).collect::<Vec<_>>());
                for c in GROUPS.iter().flat_map(|g| g.1.iter()) {
                    assert_eq!(o.matches(&format!("\n  {} ", &*c.short)).count(), 1, "{l:?}:{}", &*c.short);
                    // The first word of the short form is the name; every usage form starts with one of its names
                    assert_eq!(c.short.split_whitespace().next(), Some(c.names[0]));
                    assert!(c.usage.iter().all(|u| c.names.contains(&u.split_whitespace().next().unwrap())), "{:?}", c.names);
                }
                // The description column is aligned (Chinese counted as two cells): every line pads "  FORM" out to the same cell
                let col = |r: &str| {
                    let body = &r[2..];
                    let end = body.find("  ").unwrap();
                    2 + width(&body[..end]) + body[end..].len() - body[end..].trim_start().len()
                };
                let cols: Vec<usize> = o.lines().filter(|r| r.starts_with("  ") && r[2..].contains("  ")).map(col).collect();
                assert!(cols.len() > 20 && cols.windows(2).all(|w| w[0] == w[1]), "{l:?}:{cols:?}");
            });
        }
        assert_eq!((width("…·→"), width("中(a)"), width("「」")), (3, 5, 4));
        let o = overview();
        let at = |needle: &str| o.lines().find(|l| l.contains(needle)).map(|l| width(l.split(needle).next().unwrap())).unwrap();
        assert_eq!(at("列表:状态"), at("新建会话并启动"));
        assert_eq!(at("列表:状态"), at("打开界面"));
    }

    #[test]
    fn one_command_page_has_its_usage_its_line_and_its_details() {
        let new = of("new").unwrap();
        assert!(new.starts_with("aca new <工具[:账号]> [目录]") && new.contains("--say 话") && new.contains("例:"), "{new}");
        // Two names sharing one entry both find the same page; the usage prints only its own line
        assert_eq!(of("stop"), of("start"));
        assert_eq!(usage("stop").as_deref(), Some("aca stop <id>\n     aca stop --idle <时长>"));
        assert_eq!(usage("rename").as_deref(), Some("aca rename <id> <标题>\n     aca rename <id> --auto"));
        let output = of("output").unwrap();
        assert!(output.contains("最长 4000 字)") && output.ends_with("就是那个会话"), "{output}");
        assert!(of("keys").unwrap().contains("回车") && of("config").unwrap().contains("[accounts.claude]"));
        assert_eq!((of("nope"), usage("nope")), (None, None));
        let en = with(Lang::En, || (of("new").unwrap(), usage("rename").unwrap(), of("config").unwrap()));
        assert!(en.0.starts_with("aca new <TOOL[:ACCOUNT]> [DIR]") && en.0.contains("Examples:"), "{}", en.0);
        assert_eq!(en.1, "aca rename <id> <TITLE>\n       aca rename <id> --auto");
        assert!(en.2.contains("[accounts.claude]") && en.2.contains("lang = \"zh\""));
    }

    /// The detail pages are laid out the same in both languages: the first line of the argument table is indented like the lines
    /// after it, and no line is accidentally indented by one cell
    #[test]
    fn detail_pages_are_indented_the_same_way_in_both_languages() {
        for l in [Lang::Zh, Lang::En] {
            with(l, || {
                for c in GROUPS.iter().flat_map(|g| g.1.iter()) {
                    let page = of(c.names[0]).unwrap();
                    assert!(page.lines().all(|l| l.starts_with("  ") || !l.starts_with(' ')), "{l:?} {}", c.names[0]);
                    // Both have notes to add, or neither does
                    assert_eq!(c.more.zh.is_empty(), c.more.en.is_empty(), "{}", c.names[0]);
                }
                for name in ["group", "ls", "send", "events", "fork", "rename", "import", "cost", "meta", "plugin", "hook", "completion"] {
                    let page = of(name).unwrap();
                    let rows: Vec<&str> = page.lines().skip_while(|l| !l.is_empty()).filter(|l| l.starts_with(' ')).collect();
                    assert!(!rows.is_empty() && rows.iter().all(|l| l.starts_with("  ")), "{l:?} {name}:{rows:?}");
                }
            });
        }
    }

    #[test]
    fn a_mistyped_command_is_matched_to_the_nearest_one() {
        assert_eq!(guess("atach"), Some("attach"));
        assert_eq!(guess("renmae"), Some("rename"));
        assert_eq!(guess("rest"), Some("restart"));
        assert_eq!(guess("adop"), Some("adopt"));
        // A start that matches several, or one too far off: no guess
        assert_eq!((guess("s"), guess("xyzzy"), guess(""), guess("nope")), (None, None, None, None));
        assert_eq!(guess("l"), None);
        assert_eq!((distance("kitten", "sitting"), distance("", "ab"), distance("ls", "ls")), (3, 2, 0));
    }

    #[test]
    fn help_is_asked_for_before_the_double_dash_and_not_as_a_message() {
        assert!(asked(&words(&["--help"])) && asked(&words(&["claude", "-h"])) && asked(&words(&["x", "--help", "--", "y"])));
        assert!(!asked(&words(&["claude", "--", "--help"])) && !asked(&words(&["ab12", "-m", "--help"])) && !asked(&words(&[])));
        assert!(!asked(&words(&["claude", "--say", "-h"])) && asked(&words(&["-m", "x", "-h"])));
    }
}
