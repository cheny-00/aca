//! `aca help`:总览一屏看完;一条命令的细节是 `aca help <命令>`(`aca <命令> --help` 也到这)。
//!
//! 每条命令的写法只记在这张表里:总览里的那一行、细节页、参数给错时提示的用法,都从这出。
//! **加命令 = `cli.rs` 一个分支 + 这里一项**:表里没有的名字 `cli.rs` 不认,到不了它的分支。
//!
//! 每样都是中英两份(`Tr::new(中文, 英文)`),并排写,改一边顺手改另一边。英文的用法里占位词全大写
//! (`<TITLE>`、`-g GROUP`),小写的是原样敲的词:补全靠这个分(complete.rs),测试查两边拆出来一样。

use aca_core::i18n::{Lang, Tr, lang};
use aca_core::tr;

pub struct Cmd {
    /// 名字;共用一条说明的(start / stop)都列上
    pub names: &'static [&'static str],
    /// 总览里的写法(短的那种)和那一句话
    pub short: Tr,
    pub brief: Tr,
    /// 完整用法,一行一种,不带开头的 `aca `
    pub usage: &'static [Tr],
    /// 每个参数是什么、要知道的事、例子;空 = 没有要补充的
    pub more: Tr,
}

/// 总览里的分区,按这个顺序
pub const GROUPS: &[(Tr, &[Cmd])] = &[
    (
        Tr::new("界面", "Interface"),
        &[
            Cmd {
                names: &["ui"],
                short: Tr::new("ui [关键词] [-l]", "ui [KEYWORD] [-l]"),
                brief: Tr::new(
                    "界面(不带参数的 aca 就是它);带关键词时只剩一个匹配就直接进",
                    "the interface (plain aca); KEYWORD with one match enters it",
                ),
                usage: &[Tr::new(
                    "ui [关键词] [-l] [-n] [--client 客户端] [--follow]",
                    "ui [KEYWORD] [-l] [-n] [--client CLIENT] [--follow]",
                )],
                more: Tr::new(
                    "\
列表 + 高亮会话的实时画面,回车进去;进去之后 Ctrl+Q(或 tmux 前缀键 + d)回来。
里面按 ? 看快捷键;每个键的细节:aca help keys

  关键词             只剩一个匹配就直接进,否则打开界面并按它过滤(只找上次停在的工作区)
  -l                 只打印列表,不进界面(输出不是终端时也是这样)
  -n                 只打印会进哪个会话,不进
  --client 客户端    在 tmux 弹窗里用:回车把这台客户端切过去,打开时高亮它现在停着的会话
  --follow           弹窗只占屏幕一边时(和 --client 一起):高亮挪到哪,这台客户端就切到哪,弹窗不关;
                     ⏎ 留下,Esc 回到打开时的地方,Ctrl+\\ 关掉。绑键的写法:aca help keys
  --snapshot 宽x高 [按键]    不开终端,照这个尺寸画一帧打印出来(测试用)",
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
                    "zsh 补全:Tab 补命令、会话、分组、目录(有 fzf-tab 就是 fzf)",
                    "zsh completion: commands, sessions, groups, dirs (fzf-tab too)",
                ),
                usage: &[Tr::same("completion zsh")],
                more: Tr::new(
                    "  zsh    打出 zsh 的补全函数(现在只有 zsh 的)

装法二选一:
  aca completion zsh > ~/.zsh/completions/_aca   放进 fpath 里的一个目录,删掉 compinit 的缓存(.zcompdump)再开 shell
  eval \"$(aca completion zsh)\"                   写进 ~/.zshrc,放在 compinit 后面

补什么照每条命令的用法来:<id> 补会话(最近动过的在前,说明是标题和分组)、-g 分组 补分组和 .(按目录定)、
[目录] 补目录、子命令原样补;打了 - 才列选项。候选每次现问 aca:会话、分组、命令变了都不用重新生成。
装了 fzf-tab:fzf 里打标题、组名就找得到会话。要预览会话,~/.zshrc 里加一行
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
                brief: Tr::new("新建会话并启动(tmux 会话名就是标题)", "create a session and start it (tmux session name = title)"),
                usage: &[Tr::new(
                    "new <工具[:账号]> [目录] [-t 标题] [-g 分组] [-w 分支] [--parent id] [--notify 哪些] [--hold] [--say 话] [-- 给 agent 的参数…]",
                    "new <TOOL[:ACCOUNT]> [DIR] [-t TITLE] [-g GROUP] [-w BRANCH] [--parent id] [--notify WHICH] [--hold] [--say TEXT] [-- AGENT-ARGS…]",
                )],
                more: Tr::new(
                    "\
打印新会话的 id(标准输出只有这一行)。

  工具[:账号]    认哪些名字:aca names。账号是 config.toml 里登记的名字,或者直接一个配置目录(claude:~/.claude-x);
                 不写账号用这个工具默认的,没有默认的就什么都不加(它用它自己的配置目录和登录)
  目录           不写 = 当前目录
  -t 标题        不写 = 目录名,之后跟着 agent 自己起的标题走;写了就锁住不再跟
  -g 分组        可以带层级(work/app),没有就建。写 . = 按目录定(见 aca help group)。
                 不写:在某个会话里建的跟它进同一组,否则没分组
  -w 分支        起在这个分支的 git worktree 里:仓库的 .worktrees/<分支>(写进 .git/info/exclude,主仓库
                 git status 看不到)。分支没有就从目录现在的 HEAD 开,有就用它;在哪个 worktree 写到标准错误。
                 aca rm 时干净的 worktree 一起删,分支留着;有改动或没跟踪的文件就留着。也写 --worktree
  --parent id    挂成哪个会话的子会话。不写:在某个会话里跑这条命令就挂在它下面;写 - 不挂
  --notify 哪些  它在哪些事上通知 parent(叫醒它):done failed exited waiting 里挑(逗号隔开),all = 都要,
                 none = 都不。不写照 config.toml 的 [inbox] notify,那也没写就都不通知(aca help msg)
  --hold         只登记,不启动(之后 aca start)
  --say 话       开场那句话。Claude、Codex 当启动参数带进去:它起来了自己发,这条命令不等;
                 别的工具等它起来、空下来再打字进去(最多等一分钟)
  -- …           每次启动都原样交给 agent 的参数

例:
  aca new claude:work ~/code/app -g app -t 修登录 --say '登录页 500 了,看一下'
  aca new codex . -- --search
  aca new claude -g .            # 当前目录,进以目录名命名的组",
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
  --say TEXT      the opening message. Claude and Codex get it as a launch argument: they send it once
                  they're up, and this command doesn't wait; other tools get it typed in once up and idle
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
                    "接管不是 aca 起的 tmux 会话(不用停它);不带参数只列出能接管的",
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
接管 = 登记一行 + 在那个 tmux 会话上记个记号 + 会话名改成标题。里面的进程原样跑着,一个键都不往里按。
之后它就是普通的 aca 会话:改名、挪组、停、在原地重启、分叉。不想要了:aca release <id> 放手,进程照留。

  不带参数         列出能直接接管的(没登记过、第一格里跑着认得出的 claude / codex),只看不动
  tmux 会话        会话名、名字里的一段(只对上一个时)、$会话号;agent 不在第一格时写 %窗格号 指明是哪一格。可以给几个
  --all            列出来的全接管
  -t 标题          不写:agent-deck 起的用它那边的标题,别的用现在的 tmux 会话名(你自己起的名字会锁住)
  -g 分组          写 . = 各自按目录定(见 aca help group)。不写:agent-deck 起的进它那边的分组,别的没分组
  --as 工具[:账号] 不看进程,照这个登记(没认出来、认错了时用)
  --conv 对话id    认不出对话时自己给(手动起的 Codex 认不出:在它里面 /status 看)
  -n               只看会怎么登记,不动
  -- …             每次启动带的参数;不写就照搬它现在命令行里的(「接哪段对话」的那几个不搬)

认得出什么:哪一家看进程名;账号看进程环境里的配置目录,对回 config.toml 里登记的;Claude 现在的对话和标题读它自己记的
(配置目录里的 sessions/<pid>.json);agent-deck 起的,标题、分组、id 照它那边登记的(只读它的库)。
接管之后、重启之前它没带 aca 的 hook:状态只靠读屏幕,没有「跑完没看」。重启一次(aca restart <id>,对话接着来)就全了。
agent-deck 起的例外:它自己的 hook 还在报,「跑完没看」借它记的,不用等重启。",
                    "\
Adopting = one registry row + a mark on that tmux session + renaming the session to the title. The processes inside
keep running untouched; not a single key is sent in.
From then on it's an ordinary aca session: rename, move, stop, restart in place, fork. Don't want it any more:
aca release <id> lets go, and the processes keep running.

  (no arguments)       list what can be adopted as is (unregistered, a recognizable claude / codex in the first pane);
                       only looks
  TMUX-SESSION         session name, part of a name (when only one matches), or $session-number; when the agent isn't
                       in the first pane, %pane-number says which pane. Several are fine
  --all                adopt everything listed
  -t TITLE             default: agent-deck's sessions keep its title, others take the current tmux session name
                       (a name you chose yourself gets locked)
  -g GROUP             . = each decided by its directory (see aca help group). Default: agent-deck's go into its
                       group, others get none
  --as TOOL[:ACCOUNT]  ignore the process and register it as this (when not recognized, or recognized wrong)
  --conv CONV-ID       give the conversation when it can't be recognized (a hand-started Codex can't: /status inside)
  -n                   only show how it would be registered; change nothing
  -- …                 arguments for every start; default: copied from its current command line (minus the ones that
                       pick which conversation to resume)

What it recognizes: the agent from the process name; the account from the config directory in the process environment,
matched against config.toml; Claude's current conversation and title from its own records (sessions/<pid>.json in the
config directory); for agent-deck's sessions, title, group and id as registered there (its database is only read).
After adopting and before a restart it carries no aca hooks: state comes from the screen alone, and there is no
\"finished, unread\". Restart once (aca restart <id>, the conversation continues) and it's complete.
agent-deck's are the exception: its own hooks still report, and \"finished, unread\" comes from its records, no restart.",
                ),
            },
            Cmd {
                names: &["ls"],
                short: Tr::same("ls"),
                brief: Tr::new("列表:状态、id、标题、工具:账号、多久没动", "the list: state, id, title, tool:account, idle time"),
                usage: &[Tr::same("ls [--json] [--no-screen]")],
                more: Tr::new(
                    "  --json         打成 JSON(给脚本读)
  --no-screen    不读屏幕,只用 hook 报的状态(几毫秒;按 Esc 打断这类 hook 不报的事看不出来)

状态:● 在跑  ◐ 在等你 / 跑完没看  ○ 空闲  ✕ 出错  ■ 没在跑  ☾ 闲着被停的。在等你的下面多一行它问的。
在终端里是排好、上了色的:id 只打前 8 位,加粗的那几位就够敲(aca attach 3f);最底下一行是总数。
接到管道、文件里是不带颜色的原样,id 是完整的。",
                    "  --json         print as JSON (for scripts)
  --no-screen    don't read screens, only the state hooks report (milliseconds; misses what hooks don't report,
                 like an Esc interrupt)

States: ● running  ◐ waiting for you / finished, unread  ○ idle  ✕ error  ■ not running  ☾ stopped for idling. One waiting for you
gets an extra line with what it asked.
In a terminal it's laid out and colored: ids show their first 8 characters, the bold ones are enough to type
(aca attach 3f); the last line is the total. Piped or redirected it's plain, with full ids.",
                ),
            },
            Cmd {
                names: &["show"],
                short: Tr::same("show [id]"),
                brief: Tr::new(
                    "一个会话的全部:状态、目录、对话 id、最后一条回复",
                    "all about one session: state, dir, conversation id, last reply",
                ),
                usage: &[Tr::same("show [id] [--json]")],
                more: Tr::new(
                    "  id        不写:你在某个会话的 tmux 里另开的窗口里敲的,就是那个会话
  --json    打成 JSON(给脚本读)

在终端里最后一条回复照 Markdown 排好、代码按语言上色(aca output 也是);接到管道、文件里是原文。",
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
                    "进去(已经在 tmux 里就是切过去);Ctrl+Q 或 tmux 前缀键 + d 出来",
                    "go in (inside tmux: switch to it); Ctrl+Q or prefix + d leaves",
                ),
                usage: &[Tr::same("attach <id>")],
                more: Tr::same(""),
            },
            Cmd {
                names: &["next"],
                short: Tr::same("next"),
                brief: Tr::new(
                    "进下一个在等你的会话;在会话里按 Ctrl+Alt+N 就是它",
                    "go to the next session waiting for you; Ctrl+Alt+N inside one",
                ),
                usage: &[Tr::new("next [--session id] [--client 客户端]", "next [--session id] [--client CLIENT]")],
                more: Tr::new(
                    "\
先进在等你确认 / 选择的(!),再进跑完了还没看的(●),同一种里最近有动静的在前;你正在的那个不算。
离开的这个、进去的那个都算看过了,所以再来一次是下一个。没看过的没有了、还有在问的:在它们之间轮,最久没去的先去。
一个都没有:说一句「没有在等你的」,人不动。

键:aca 绑在 tmux 上(和 Ctrl+Q 一个办法,不用前缀,不写进你的 tmux.conf),只在 aca 的会话里拦,别的会话里键照样送给
里面的程序;往上翻着看的时候也能按。默认 Ctrl+Alt+N,记在 config.toml 的 [keys] next;换:aca tmux key next <键>。

  --session id       你正在的那个会话(键按下去时 tmux 填的);不写:在 tmux 里敲的就是敲的那个会话
  --client 客户端    切哪一台 tmux 客户端(键按下去时 tmux 填的);不写:敲命令的这台。不在 tmux 里:连上去,detach 回来",
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
                    "启动(接着原来的对话)/ 停下(agent 退出,窗格和登记留着)",
                    "start, resuming the conversation / stop: the agent exits",
                ),
                usage: &[Tr::same("start <id>"), Tr::same("stop <id>"), Tr::new("stop --idle <时长>", "stop --idle <DURATION>")],
                more: Tr::new(
                    "  stop           让 agent 自己退出(3 秒不走再强制)。它那一格连最后的画面留着,同一个 tmux 会话里另开的窗口不动。
                 列表里标 ■
  --idle 时长    不写 id:停掉空闲了这么久的会话(30m、3h、1d、1h30m),列表里标 ☾,和一个个停的 ■ 分开。
                 空闲从最后一次有动静算起:屏幕有输出(干活时转圈、计时一直在刷)、有人进去过、hook 报事、对话记录
                 (连同子代理的)有写入。不停:这一轮没跑完的(hook 只报了开头,屏幕看着再闲也不停)、跑完没看的、
                 在等你的、出错的、有人连着的、有后台任务的、输入框里有没发的字的、派出去的会话还开着的、
                 给自己定了还没到点的(Claude 的 CronCreate、/loop 的下一次:只在它进程里,停了就没了)、
                 再起接不上原来对话的。打出停了哪些
  start          没在跑就在原地再起,接着原来的对话(起了没说过话的是新的一段);tmux 里整个没了(重启过电脑)就重新建

自动停:config.toml 里写
  [idle]
  stop_after = \"3h\"
aca 界面开着时每 5 分钟照 stop --idle 3h 停一次(你连进哪个会话里了也照看)。",
                    "  stop           ask the agent to exit (forced after 3 seconds). Its pane stays with the last screen; other
                 windows in the same tmux session are left alone. Drawn ■ in the list
  --idle DURATION  without an id: stop the sessions idle this long (30m, 3h, 1d, 1h30m), drawn ☾ in the list,
                 apart from the ■ of ones stopped one by one. Idle counts from the last sign of life: output on screen
                 (a working agent's spinner and timer keep redrawing), someone attaching, a hook, a write to its
                 conversation record (or a subagent's). Never stopped: a turn that hasn't finished (the hook reported
                 only its start, however idle the screen looks), finished-unread, waiting for you, failed, attached,
                 background tasks, unsent text in its input box, dispatched sessions still open, a schedule of its own
                 not yet due (Claude's CronCreate, /loop's next wakeup: they live only in its process), or one that
                 can't resume its conversation. Prints the ones it stopped
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
                    "原地重启接着对话;--fresh 换一段新的;--as 换账号接着",
                    "restart in place, resuming; --fresh anew, --as other account",
                ),
                usage: &[Tr::new("restart <id> [--fresh] [--as 工具[:账号]]", "restart <id> [--fresh] [--as TOOL[:ACCOUNT]]")],
                more: Tr::new(
                    "  --fresh          不接原来的对话,换一段新的
  --as 工具[:账号] 换个账号接着这段对话(一个账号额度用完了、活还没干完):对话记录从原账号的配置目录拷到
                   新账号的同一位置,上下文原样带过去,行、tmux 会话、标题都不动;原账号那份留着,之前那几轮
                   计费还算它的。只写一个词:是这个工具的账号就当账号,不是就当工具名(用它默认的账号)。
                   只能在同一家里换(Claude 和 Codex 的对话记录不通,换家用界面里的 H 交接)。
                   Codex 各账号默认模型不一样时带上原来的 -m。新账号的缓存是空的:头一轮整段上下文重新写一遍",
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
                    "从现在的对话分出一个新会话(分组、账号跟原来的,排在它后面)",
                    "branch a new session off its conversation, listed after it",
                ),
                usage: &[Tr::new("fork <id> [-t 标题]", "fork <id> [-t TITLE]")],
                more: Tr::new(
                    "  -t 标题    不写 = 「原标题 (分叉)」

打印新会话的 id。要有过对话才分得了。
分出来的是你自己接着聊的另一条路,和原会话平级:不算它派出去的(没有 parent,不通知它),分自谁记在 fork_of。",
                    "  -t TITLE    default: \"<title> (fork)\"

Prints the new session's id. Same group and account as the original. Needs a conversation to fork.
The fork is another line of your own, a peer of the original: not one it dispatched (no parent, it isn't
notified); where it came from is recorded as fork_of.",
                ),
            },
            Cmd {
                names: &["rename"],
                short: Tr::new("rename <id> <标题>", "rename <id> <TITLE>"),
                brief: Tr::new("改名:登记、tmux 会话名、窗口名一起改", "rename: registry, tmux session and window names together"),
                usage: &[Tr::new("rename <id> <标题>", "rename <id> <TITLE>"), Tr::same("rename <id> --auto")],
                more: Tr::new(
                    "  --auto    放开名字,重新跟 agent 自己起的标题

改过名之后就锁住,不再跟 agent 自己起的标题。",
                    "  --auto    release the name and follow the agent's own title again

Once renamed the name is locked and no longer follows the agent's own title.",
                ),
            },
            Cmd {
                names: &["move"],
                short: Tr::new("move <id> <分组>", "move <id> <GROUP>"),
                brief: Tr::new("挪到别的分组(没有就建),排到那一组最后", "move to another group (created if missing), at its end"),
                usage: &[Tr::new("move <id> <分组>", "move <id> <GROUP>")],
                more: Tr::new(
                    "分组写 '' 是挪出分组;写 . 是按它的目录定的组(见 aca help group),打出进了哪个。",
                    "GROUP '' moves it out of any group; . is the group its directory decides (see aca help group), printed.",
                ),
            },
            Cmd {
                names: &["order"],
                short: Tr::new("order <id> <位置>", "order <id> <POS>"),
                brief: Tr::new("组里的先后(一个数,小的在前)", "position within its group (a number, smaller first)"),
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
                    "  ls        全部分组:先后、路径、默认目录
  --json    打成 JSON(给脚本读)
  add       把目录(不写 = 当前目录)登记成分组,名字按目录定(见下面),打出组名;已经有它的组就只打出来
  create    自己起名,连上级一起建。默认目录:界面里在这个组新建会话时先填的路径
  rename    改名,或者挪到别的上级下面(fun/aca);子分组、里面的会话跟过去,界面里记着它的(工作区、折叠)也跟着改。
            界面里停在分组上按 r 也是它,接着还能改默认目录
  dir       看 / 改默认目录,'' = 不记了。一个目录只当一个组的默认目录
  rm        子分组一起删;里面的会话不停不删,挪到上一级
  order     分组的先后(一个数,小的在前)

按目录定组(group add,和 new / adopt / move 里的 -g . 或分组写 .;界面里 N 改了路径也是这一套):
  1. 哪个组的默认目录就是它 → 那个组
  2. 目录名已经是分组(顶层同名的,或者唯一一个「…/目录名」的嵌套组)→ 那个组
  3. 上一级目录名是分组 → 嵌在它下面:「那个组/目录名」
  4. 都不是 → 顶层的「目录名」
  2–4 定下的组记着别的目录 → 往后编号:…-2、-3。定下的组没记默认目录的,记成这个目录。
  名字不合适:aca group rename 改

往分组里放会话:aca new … -g <路径>、aca move <id> <路径>。",
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
                brief: Tr::new("往它的输入框发一段话并回车", "type text into its input box and press Enter"),
                usage: &[Tr::new("send [id] [-m 话] [--wait 秒]", "send [id] [-m TEXT] [--wait SECS]")],
                more: Tr::new(
                    "  id           不写:你在某个会话的 tmux 里另开的窗口里敲的,就是那个会话
  -m 话        不给就读标准输入(多行也行:是粘进去的,不会被当成发了几次)
  --wait 秒    它在跑 / 在等你时先等它空下来,最多等这么久,到时间还没空就不发(退出码 1);不写 = 不等,直接发

没在跑的发不了。不带 --wait 时它在等确认:不打(回车可能直接选中默认那一项),报错;要替它回答用 aca answer。
给别的会话递话(不碰它的输入框、它不方便时先存着)用 aca msg。",
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
                brief: Tr::new("最后一条回复(hook 记的,最长 4000 字)", "the last reply (recorded by hooks, up to 4000 characters)"),
                usage: &[Tr::same("output [id]")],
                more: Tr::new("  id        不写:你在某个会话的 tmux 里另开的窗口里敲的,就是那个会话", "  id        default: typed in a window you opened inside a session's tmux, that session"),
            },
            Cmd {
                names: &["seen"],
                short: Tr::same("seen <id>"),
                brief: Tr::new("标成看过了(◐ 跑完没看 → ○)", "mark as seen (◐ finished, unread → ○)"),
                usage: &[Tr::same("seen <id>")],
                more: Tr::same(""),
            },
            Cmd {
                names: &["archive", "unarchive"],
                short: Tr::same("archive | unarchive <id>"),
                brief: Tr::new(
                    "归档(tmux 会话关掉,登记留着,列表里不再列)/ 取回",
                    "archive (tmux session closed, row kept, unlisted) / restore",
                ),
                usage: &[Tr::same("archive <id>"), Tr::same("unarchive <id>")],
                more: Tr::new(
                    "取回之后是停着的:aca start <id> 接着原来的对话起来。",
                    "Restored it's stopped: aca start <id> brings it up resuming the conversation.",
                ),
            },
            Cmd {
                names: &["release"],
                short: Tr::same("release <id>"),
                brief: Tr::new(
                    "放手:只删登记,tmux 会话和里面的进程照留(adopt 的反面)",
                    "let go: unregister only, tmux and processes stay (un-adopt)",
                ),
                usage: &[Tr::same("release <id>")],
                more: Tr::new(
                    "它变回一个普通的 tmux 会话(会话名不改回去)。想再要:aca adopt。",
                    "It's an ordinary tmux session again (the name isn't changed back). Want it back: aca adopt.",
                ),
            },
            Cmd {
                names: &["rm"],
                short: Tr::same("rm <id>"),
                brief: Tr::new(
                    "删除:tmux 会话关掉(里面另开的窗口一起),登记和状态都删",
                    "delete: close its tmux session (all windows), drop its records",
                ),
                usage: &[Tr::same("rm <id>")],
                more: Tr::new(
                    "对话记录是 agent 自己的,不动。只想删登记、让它接着跑:aca release。",
                    "Conversation records are the agent's own and stay. To only unregister and keep it running: aca release.",
                ),
            },
        ],
    ),
    (
        Tr::new("会话之间递话", "Between sessions"),
        &[
            Cmd {
                names: &["msg"],
                short: Tr::new("msg <id | parent> <话>", "msg <id | parent> <TEXT>"),
                brief: Tr::new(
                    "给别的会话递一句话:放进它的收件箱,挑它方便的时候送进去",
                    "pass a message: into its inbox, delivered when it's convenient",
                ),
                usage: &[Tr::new("msg <id | parent> [话…]", "msg <id | parent> [TEXT…]")],
                more: Tr::new(
                    "\
  id             发给谁:会话 id 或名字
  parent         派你的那个(在某个会话里发时)
  话             不写就读标准输入
在某个会话里发的,发件人是那个会话;在外面发的是「你」。

怎么送进去看对方是哪一家,都不碰它的输入框:
  Claude    按门铃:它自己盯着一个文件,一变就把消息取走,当成系统提示给模型。空着马上开一轮,
            在跑就塞进这一轮,在等确认就等确认完再送
  Codex     排进它自己的队列(codex queue):空着 10 秒内自己开一轮,在跑就等这一轮跑完。
            你刚按 Esc 打断它时不去催,等你下一轮跑完再送
  打字      上面两样用不了时(接管来、没接上门铃的 Claude;还没说过话的 Codex;别的 agent):
            它空着、你没在那一格打字、输入框里没有你没发的字才打,不然过一阵再看
送不了的先存着,等它这一轮跑完、重新起来时再送。shell 会话不收。
排队、打字进去的就是那句话:你发的原样,别的会话发的前面多一行「[aca] 来自会话 …」,不往 agent 里加别的东西。

派它的时候说了要(aca new --notify done,或者 config.toml 的 [inbox] notify),被派出来的会话在跑完一轮、
出错、退出、在等确认时,自己的 hook 会给 parent 发一条通知,不用谁一直盯着。没说就不发:不开口不叫醒谁。
同一个会话 10 分钟里给同一个会话发了 20 条以上,后面的只存不叫醒。",
                    "\
  id             who to: a session id or a name
  parent         the one that dispatched you (when sent from inside a session)
  TEXT           default: read standard input
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
                    "收件箱:收到了什么、送到了没有;不给 id 就列出有没送的会话",
                    "inbox: what came, delivered or not; no id: who has undelivered",
                ),
                usage: &[Tr::new("inbox [id] [-n 条数] [--json]", "inbox [id] [-n COUNT] [--json]")],
                more: Tr::new(
                    "  id        不写:在某个会话里就是它自己的;在外面就列出哪些会话有没送的
  -n 条数   最近几条(默认 20)
  --json    一行一条 JSON(给脚本读)

[没送] 还在收件箱里  [排着] 排进了 Codex 的队列,它还没为这条开一轮  [送到了] 到了它眼前",
                    "  id         default: inside a session, its own; outside, which sessions have undelivered messages
  -n COUNT   the latest few (default 20)
  --json     one JSON message per line (for scripts)

[undelivered] still in the inbox  [queued] in Codex's queue, no turn for it yet  [delivered] in front of it",
                ),
            },
            Cmd {
                names: &["wait"],
                short: Tr::same("wait <id>…"),
                brief: Tr::new(
                    "等它跑完一轮 / 在等确认 / 出错 / 退出;退出码说是哪种",
                    "wait for a turn to end / a confirmation / failure / exit",
                ),
                usage: &[Tr::new("wait <id>… [--any] [--timeout 秒]", "wait <id>… [--any] [--timeout SECS]")],
                more: Tr::new(
                    "  --any          等到一个就回;不写等全部
  --timeout 秒   最多等这么久(退出码 124)

退出码:0 跑完一轮  2 在等确认  3 出错  4 退出了 / 停了  124 到时间了。几个一起等时,是第一个不是 0 的。
出错的那一行写着是哪一类:临时的(限流、过载、断流:过一会儿 aca send 让它接着干)、账号的、再试也一样的。
Codex 一轮出错不报 hook:aca 等的时候从它的对话记录里读出来。
刚发给它的话还没送到、还没变成「在跑」的,不算跑完。
在某个会话里调的:等到的那几个发来的通知记成送到了,不会再为它们被叫醒一次。",
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
                    "替在等确认的会话回答:选择题按选项号,权限框 yes / no",
                    "answer for one waiting: a choice's number, or yes / no",
                ),
                usage: &[Tr::same("answer <id>"), Tr::new("answer <id> <选项号 | yes | no>", "answer <id> <NUMBER | yes | no>")],
                more: Tr::new(
                    "\
不带回答:只看它在问什么、有哪几个选项,或者要跑什么命令。
  选项号      选择题(一次只问一题、不能多选的):按下那个数字
  yes         权限框里同意:Claude 按 1
  no          权限框里拒绝:Claude 按 Esc。权限框的选项个数和顺序会变,所以只做这两样
  选项号      启动时的关卡(信任这个目录、有更新、hook 要审这类只在屏幕上的框):屏幕上有一串带编号的选项才认,
              用方向键走到那一项再回车

按之前先核对它还在问同一句,不对就不按。按完往事件流记一条 ask.answered(谁替谁答的、答了什么)。
在某个会话里调的,只能答自己派出去的那些(parent 是它,或者再往下几层)。",
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
                    "中止它正在跑的这一轮(按 Esc):往错的方向跑了的时候",
                    "stop its current turn (Esc): when it went the wrong way",
                ),
                usage: &[Tr::same("interrupt [id]")],
                more: Tr::new(
                    "\
id 不写:你在某个会话的 tmux 里另开的窗口里敲的,就是那个会话。
只在它真在跑时按;在等确认的不按(Esc 在确认框里是拒绝,要拒绝用 aca answer <id> no)。
停下以后它空着:换个说法让它接着干用 aca send <id> -m '…'(打字进去马上开一轮)。
Codex 被打断后,排在它队列里的话不会自己开跑,也要用 aca send。
在某个会话里调的,只能中止自己派出去的那些。",
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
                brief: Tr::new("new 认的名字:工具,和「工具:账号」", "names new accepts: tools, and TOOL:ACCOUNT"),
                usage: &[Tr::same("names")],
                more: Tr::new(
                    "账号、工具在 config.toml 里登记,写法:aca help config。",
                    "Accounts and tools are registered in config.toml; how to write it: aca help config.",
                ),
            },
            Cmd {
                names: &["account"],
                short: Tr::new("account add <claude|codex> <名字>", "account add <claude|codex> <NAME>"),
                brief: Tr::new(
                    "登记账号(名字 = 配置目录);status 登录着没有,rm 删登记",
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
                    "  ls              登记了的账号:工具:名字、配置目录(不带参数也是它)。顺序是界面账号页里排的那个
  status          各账号登录着没有:问的是工具自己(claude auth status / codex login status),aca 不读凭据;
                  配置目录不在的、目录其实是另一家的(把 ~/.codex 登记成了 Claude 的账号)也在这说
  add             登记一个:往 config.toml 的 [accounts.claude] / [accounts.codex] 加一行「名字 = 目录」,
                  别的内容和注释不动;目录没有就建。目录是另一家的配置目录的不给登记
  rm              删一个登记:只去掉 config.toml 里那一行(删之前的存成 config.toml.bak-aca),
                  配置目录和里面的登录都不动。有会话在用、或者是哪个工具的默认账号的,不删,说是谁在用
  --force         rm:有会话在用、是默认账号的也删
  claude          Claude 的账号(起会话时这个目录是它的 CLAUDE_CONFIG_DIR)
  codex           Codex 的账号(这个目录是它的 CODEX_HOME)
  名字            你起的(字母、数字、- 和 _),起会话时写成「claude:名字」
  目录            它的配置目录;不写就是 ~/.claude-<名字> / ~/.codex-<名字>
  --statusline    只对 Claude 的账号:登记完顺手把它的状态栏接上(不写就不接)
  aca             状态栏交给 aca statusline 画
  wrap            留着它自己的状态栏,前面套一层 aca limits 只记额度

aca 不扫、不猜有哪些账号,也不碰登录:新建的目录要登录一次,用工具自己的登录,命令会打出来
(aca run claude:<名字>,Codex 是 aca run codex:<名字> -- login)。不登记也能用:就是 ~/.claude、~/.codex。
想对着一张表挑:aca setup。界面里是设置页(^S)的 a:登录状态、加、删、⇧↑ ⇧↓ 排顺序都在那一页。",
                    "  ls              registered accounts: tool:name, config directory (also with no arguments), in the order
                  set on the interface's accounts page
  status          whether each account is logged in: asks the tool itself (claude auth status / codex login status),
                  aca reads no credentials; also says when a config directory is missing or belongs to the other
                  tool (~/.codex registered as a Claude account)
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
                    "带着这个账号的环境跑它自己的命令(login、mcp …),不建会话",
                    "run the tool's own commands (login, mcp …) as that account",
                ),
                usage: &[Tr::new("run <工具[:账号]> [-- 参数…]", "run <TOOL[:ACCOUNT]> [-- ARGS…]")],
                more: Tr::new(
                    "\
和在终端里敲 CODEX_HOME=~/.codex2 codex login 是一回事,只是不用把目录再敲一遍。
带的是账号的环境;参数只有你给的(工具、账号里登记的 args 是起会话用的,这里不带)。
跑的是登录(claude 的 auth login、codex login)时不替你开默认浏览器:
链接工具自己打在屏幕上,贴到要用的浏览器里;你自己设了 BROWSER 的,Claude 用你的。
从 SSH 连进来时换成在别的设备上登得完的:Claude 给要把码贴回终端的链接,Codex 加 --device-auth。
在界面的账号页按 ⏎ 登录,链接出来会弹一张表:复制链接,或者挑一个浏览器开。

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
                    "把 agent-deck 登记的分组和会话搬过来(只读它的库)",
                    "bring over agent-deck's groups and sessions (read-only)",
                ),
                usage: &[Tr::same("import agent-deck [--apply] [--all]")],
                more: Tr::new(
                    "  agent-deck    从哪搬(现在只认它)
  --apply       真的写进来;不写只列出会搬哪些
  --all         那边还在跑的也登记(登记成停着的:别两边同时开同一段对话);不写就跳过

搬的是登记(标题、分组、顺序、账号、对话 id),id 沿用那边的;搬过来是停着的,aca start <id> 接着对话起来。
那边还在跑的:要么在那边停掉再搬,要么不停它直接接过来(aca adopt)。",
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
        Tr::new("用量、配色、放到外面去的", "Usage, colors, beyond this machine"),
        &[
            Cmd {
                names: &["cost"],
                short: Tr::same("cost [1|7|30|all]"),
                brief: Tr::new(
                    "计费:本机 Claude / Codex 的用量按 API 价折成美元,加订阅额度",
                    "spend: local Claude / Codex usage at API prices, plus quotas",
                ),
                usage: &[Tr::new("cost [1|7|30|all] [--only 账号,…]", "cost [1|7|30|all] [--only ACCOUNT,…]")],
                more: Tr::new(
                    "  1         今天
  7         近 7 天(不写就是它)
  30        近 30 天
  all       全部记录(要读一两秒)
  --only    只算这几个账号(逗号隔开,也可以给几次)

同界面里的 $。只读本地的对话记录,不联网。",
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
                    "给 Claude 的 statusLine 用:记下额度,输入原样交给后面的命令",
                    "for Claude's statusLine: record quotas, pass input to COMMAND",
                ),
                usage: &[Tr::new("limits [-- 命令…]", "limits [-- COMMAND…]")],
                more: Tr::new(
                    "\
想留着自己的状态栏时用:在 Claude 的 settings.json 里把 statusLine 的命令写成「aca limits -- 原来的命令」。
记下的额度显示在计费面板顶上。状态栏也交给 aca 画的话用 aca statusline,不用这条。

不过状态栏的记法:界面 ^S 的 m 打开(config.toml 的 [tools.claude] mod = true),aca 起 Claude 时带上自己的 mod,
由它在 Claude 里调 aca limits --mod(你不用敲)。见 aca help config。",
                    "\
For keeping your own status bar: in Claude's settings.json make the statusLine command \"aca limits -- <yours>\".
The recorded quotas show at the top of the billing panel. To have aca draw the status bar too, use aca statusline.

To record them without the status bar: turn on m in ^S of the interface ([tools.claude] mod = true in config.toml) and aca starts
Claude with its own mod, which calls aca limits --mod inside Claude (you don't type it). See aca help config.",
                ),
            },
            Cmd {
                names: &["setup"],
                short: Tr::same("setup [--statusline aca|wrap|off]"),
                brief: Tr::new(
                    "装完之后要定的:登记账号、各账号接不接 Claude 的状态栏(一张表)",
                    "after installing: register accounts, wire Claude status bars",
                ),
                usage: &[Tr::same("setup [--statusline aca|wrap|off]")],
                more: Tr::new(
                    "  --statusline  不开表,所有 Claude 账号都照后面这个接:
  aca           状态栏交给 aca statusline 画
  wrap          留着你自己的状态栏,前面套一层 aca limits 只记额度(没有状态栏的账号不动)
  off           撤掉 aca 接的,恢复成接之前的样子

不带参数是一张表:一行一个账号(登记了的,和没登记的 ~/.claude),Claude 的账号右边是状态栏的接法。
  ↑↓ 挑账号 · ←→ 换接法(aca 画 / 自己的 + 记额度 / 不接)· a 都照这一行 · + 登记账号(名字、配置目录)· x 这次登记的不要了
  ⏎ 照办:先登记账号,再改状态栏(只动挑的和现在不一样的),办了什么、新目录怎么登录打出来;Esc 什么都不动
不在终端里跑就只列出来。不开表登记账号:aca account add。界面里设置页(^S)的账号页按 s 是同一张表。

Claude 只在状态栏的输入里给订阅额度,所以要在它每个账号的 settings.json 里把 statusLine 指到 aca。
只换 statusLine 那一项,文件别的地方一个字不动;你原来那份记在 <state>/setup/statusline.json,不接 / off 照它恢复。
开着的 Claude 会话马上就换,不用重启。",
                    "  --statusline  no table; wire every Claude account the way that follows:
  aca           the status bar is drawn by aca statusline
  wrap          keep your own status bar, wrapped in aca limits just to record quotas (accounts without one: untouched)
  off           undo what aca wired, back to how it was before

Without arguments it's a table: one row per account (the registered ones, and an unregistered ~/.claude); Claude
accounts show how their status bar is wired on the right.
  ↑↓ pick an account · ←→ change wiring (aca draws / yours + quotas / none) · a: all like this row · + register one · x: undo that
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
                    "Claude 的状态栏:上下文、模型、分支和改动、目录,加账号的额度",
                    "Claude's status bar: context, model, branch, dir, plus quotas",
                ),
                usage: &[Tr::same("statusline")],
                more: Tr::new(
                    "\
在 Claude 的 settings.json 里把 statusLine 的命令写成「aca statusline」(想让闲着的会话也跟着变,加上 \"refreshInterval\": 秒数)。
闲着(对话记录 30 秒没动)又没人连着看的会话照上次画的原样打:不然它跟着别的会话一起重画,屏幕一直有输出,自动停等不到它闲下来。
连进去看,下一次刷新就是新的。
两行:上下文 │ 模型 │ 分支和改动 │ 目录;额度 │ 花费和时长 │ Python │ node。没有的那段不画。
额度照 aca limits 一样记下,不用再套一层;写的是这个账号所有会话里最新的那份,用到八成以上的带上还有多久重置。
分支每次直接读仓库的 HEAD;改了几个文件、几行、领先落后按目录缓存 5 秒。配色是 Catppuccin Latte,图标要 Nerd Font。",
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
                    "切换器的键、状态栏上几个在等你:装 / 卸;换进下一个的键",
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
  不带参数     看装没装;你的 tmux.conf 里还有没有自己手写的 aca 的行
  install      写 ~/.config/aca/tmux.conf(prefix + a 整屏开切换器,不按前缀 Ctrl+\\ 贴右边开,状态栏接上 tmux-status),
               你的 tmux.conf 里加两行:一行注释、一行 source-file -q 它(放在 TPM 那一段前面,没有就在最后)。开着的 tmux 马上用上
  uninstall    拿掉那两行、删掉 aca 那份;开着的 tmux 里 aca 绑的键、状态栏那一段也撤掉
  key          aca 自己绑在 tmux 上、你能换的键:现在是哪个、绑上没有。它们不在上面那份配置里:和 Ctrl+Q 一样
               由 aca 直接绑,没 install 也有。现在就一个:
  next         进下一个在等你的(aca help next),默认 Ctrl+Alt+N。后面给一个键就换成它(tmux 的键名:M-n、C-M-n、F7),
               记在 config.toml 的 [keys] next,马上绑上
  --none       不要这个键(aca next 照样能敲)
  tmux-status  状态栏上那一段,打出来像「 ! 2  ● 3」:只有种类和个数。! 在等你确认 / 选择(派出去的 worker 也算),
               agent 停着等你,红底;● 跑完了还没看(只算你自己开的),次要的字色。名字不打:是谁、在问什么在列表里,
               过去按 Ctrl+Alt+N(aca next)。后面 ⚠ bark 是出错了的插件(钩子、定时上次没跑成,gateway 的服务停下了;
               哪一处见 aca plugin ls)。id 是你正看着的会话(#{@aca_id}),不算它;都没有就什么都不打。
               tmux 每隔 status-interval 秒跑一次,所以只读状态文件和一条 tmux list-panes,不读屏幕:偶尔比列表慢一拍

aca 那份归 aca:每次 install 照新的重写,升级以后打开界面(^L 重启也算)时也会,./install.sh 升级时当场就重写。
别在里面改;要换键,在你的 tmux.conf 里 source 它的那一行后面自己 unbind / bind。
你的 tmux.conf 只加、只删那两行,别的不动;是链接的写到它指着的文件。
-q:aca 删了、那份没了,tmux 也不报错。不想装、自己接状态栏:set -ag status-right \"#(aca tmux-status #{@aca_id})\"",
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
  tmux-status  the status-bar segment, like \" ! 2  ● 3\": kinds and counts only. ! waits for your confirmation /
               choice (dispatched workers count too): an agent is stopped until you answer, so it's a red block;
               ● finished and not looked at (only the ones you started), in a minor color. No names: who and what they
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
                brief: Tr::new("界面的配色:看、换、新建一套", "interface colors: show, switch, create a theme"),
                usage: &[
                    Tr::new("theme [名字]", "theme [NAME]"),
                    Tr::new("theme use <名字>", "theme use <NAME>"),
                    Tr::new("theme new <名字> [从哪套]", "theme new <NAME> [FROM]"),
                ],
                more: Tr::new(
                    "\
不带参数:现在用的那套的每种颜色(名字、色块、用在哪)和所有主题;带名字看那一套。
  use    换成那套(记在 config.toml 的 [theme])
  new    在 ~/.config/aca/themes/ 生成一份主题文件(全注释掉,去掉 # 改)

环境变量 ACA_THEME=主题名 压过配置(dark / light 是 mocha / latte)。",
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
        Tr::new("给插件和脚本用的", "For plugins and scripts"),
        &[
            Cmd {
                names: &["events"],
                short: Tr::same("events [-f]"),
                brief: Tr::new("事件流:会话上发生的每件事,一行一个 JSON", "event stream: everything that happens to sessions, JSON lines"),
                usage: &[Tr::new("events [-f] [-n 行数]", "events [-f] [-n LINES]")],
                more: Tr::new(
                    "  -f         一直跟着读
  -n 行数    先打最近几行(默认 20;带 -f 时默认不打)

有哪些事件、各带什么字段:docs/plugins.zh-CN.md。",
                    "  -f          keep following
  -n LINES    print the last few lines first (default 20; with -f, none by default)

Which events there are and their fields: docs/plugins.md.",
                ),
            },
            Cmd {
                names: &["meta"],
                short: Tr::same("meta set | rm"),
                brief: Tr::new("挂在会话行上的一小段字(PR 号、CI 状态 …)", "a short tag on a session's row (PR number, CI status …)"),
                usage: &[
                    Tr::new("meta set <id> <名字> <值> [--ttl 秒]", "meta set <id> <NAME> <VALUE> [--ttl SECS]"),
                    Tr::new("meta rm <id> <名字>", "meta rm <id> <NAME>"),
                ],
                more: Tr::new(
                    "  set      挂上(同名的换掉)。名字:1–32 个字母、数字、_ -;值截到 80 个字
  rm       摘掉
  --ttl    过了这么多秒不再显示;不写 = 一直挂着",
                    "  set      attach it (replacing one with the same name). NAME: 1–32 letters, digits, _ -; VALUE: cut at 80 characters
  rm       remove it
  --ttl    stop showing it after this many seconds; default: stays",
                ),
            },
            Cmd {
                names: &["plugin"],
                short: Tr::same("plugin install | ls | run | …"),
                brief: Tr::new(
                    "插件:装、删、开关、调它的动作、开它的窗格(界面里按 P 管)",
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
                    Tr::new("plugin run <插件.动作> [名字=值…] [--session id]", "plugin run <PLUGIN.ACTION> [NAME=VALUE…] [--session id]"),
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
插件 = 一个目录 + 一份 aca-plugin.toml(能力、事件钩子、定时、窗格、设置、给 agent 带的目录和 MCP),回头调 aca 的命令行。
写法:docs/plugins.zh-CN.md。要哪些插件、给哪些 agent、设置写在 ~/.config/aca/plugins.toml;下面这些命令也会改它。
不想建插件:~/.config/aca/user/aca-plugin.toml 写上就有(id 总是 user,不用 link),删了就没。

  install        git 的克隆到 ~/.local/share/aca/plugins/<id>;owner/repo 是 GitHub 上的;本地目录就是 link;
                 aca.名字 是 aca 自带的(aca.delegate、aca.core、aca.gateway、aca.remote、aca.tailnet、aca.bark、
                 aca.discord、aca.notify、aca.copy;界面 P 页「可装」里列着),跟着 aca 升级。
                 装完列出它带了什么:给 agent 的、插件菜单里的(会话列表上按 :)、订的事件、设置
  --ref          钉在一个标签 / 分支 / 提交上(update 不动它)
  update         git 装的拉新的(只快进);不给 id = 全部。新的提交记进 plugins.lock
  sync           让本机跟 plugins.toml 一致:缺的装(git 的照 plugins.lock 的提交)、没写的删、开关照它
  gateway        MCP gateway(插件 aca.gateway 开着时):每个 MCP 只起一份,agent 只挂它一个。不写 = status
  status         在不在跑、谁连着、下面每个服务的样子、最近的调用
  stop           停掉它(下一个 agent 连上来时再起)
  restart        重启下面一个服务(不写名字 = 全部),下次有人要时再起
  log            一个服务的日志(它的 stderr)
  add            把 config.toml 的 [mcps.<名字>] 收进 gateway(改 plugins.toml 里 gateway 的设置 servers)
  drop           移出 gateway
  import         各账号里散装的 MCP(Claude 的 .claude.json、Codex 的 config.toml)列出来;
  --apply        收进来:写进 aca 的 config.toml [mcps.*] 再收进 gateway。账号里的原样留着,删不删你定
  off            只对一个会话关掉 gateway 下面的一个服务(Claude 马上换,Codex 重启后)
  on             再打开
  remove         不再用它:git 装的连目录删掉,链接的目录不动,它的热键解掉;插件自己的配置、状态留着(空目录不留)
  enable         打开
  disable        先停着(不跑它的钩子、动作,不给 agent 带它的目录),enable 再开;agent 下次启动才变
  log            看它的日志(事件钩子、动作的输出)
  new            建一个新插件:清单(一条命令、一个键、一个设置,事件钩子写好了注释着)和它跑的脚本,link 上就能试
  --id           它的 id;不写 = me.<目录名>
  check          查一个插件(目录或者链接过的 id;不写 = 当前目录),不用先链接:清单写得对不对、命令要跑的程序和脚本
                 在不在、needs 里的程序这台机器上有没有;有毛病退出码是 1
  emit           假装来了一个事件:订了它的钩子在前台跑完、输出直接打出来(平时钩子不等、输出进日志)。
                 测钩子不用真跑一轮 agent;字段=值 是事件自己的字段(kind=transient)
  字段=值        事件的字段:数、true / false 照类型,别的当字
  --plugin       只跑这个插件的钩子
  timer          定时(清单的 [[timers]],到点 launchd 叫起来跑):不写 = 列出每个开没开、每隔多久、上次跑得怎么样
                 (--json 一行一个)。on / off 开关(manual 的平时由插件自己开关);run 现在跑一次,像到点了一样
                 (输出打出来,关着也跑)
  link           链接一个插件目录(读它的 aca-plugin.toml)
  unlink         取消链接(目录不动)
  ls             插件:● 开着 ○ 关着、名字版本、从哪来、给哪家 agent 带东西
  actions        开着的插件的动作、窗格和视图
  --json         一行一个动作的 JSON(给脚本读)
  run            调一个动作
  名字=值        动作的参数(清单里 args 写了哪些),照它写的类型检查
  view           跑一个视图,打出它的输出(界面里是放进全屏阅读页)
  setting        插件的设置:不给名字列全部;给名字打出它现在的值(脚本、agent 读它);再给值就改(写进 plugins.toml)
  --reset        回到清单里的默认值
  allow          给 agent 这几个能力当工具(写进 plugins.toml 的 tools);不写能力 = 列出能给的(✓ 给了)。
                 清单里写 agent? 的(会动你机器上东西的)默认不给,要在这里点名。开着的会话重启才变
  deny           不给了
  key            : 菜单里的键:不给插件列全部(没拿到键的点名);给了就把这一个(能力 / 窗格 / 视图)换成这个键
                 (写进 plugins.toml 的 keys)。两个插件要同一个键时你定的先拿
  --none         不要键(: 菜单里挑)
  hotkey         会话里面的热键(不用前缀,像 Ctrl+Q;往上翻着看时也能按):按了开插件的一个窗格。aca 把它绑在 tmux 上,
                 只在 aca 的会话里拦,别的会话里键照样给里面的程序;插件停了、删了它就解掉,不用动 tmux.conf。
                 不给插件 = 列全部(没绑上的点名);给了就把这个窗格换成这个键(tmux 的键名:C-]、M-y、C-M-y、F5;
                 写进 plugins.toml 的 hotkeys)。清单没给热键的窗格也能给一个;你自己在 tmux 里绑了别的的键不动它
  pane           开它的一个窗格
  --session      冲着哪个会话;不写 = 现在所在的会话(agent 环境里的 ACA_SESSION_ID;你在它的 tmux 会话里另开的窗口也算)
  --placement    窗格开在哪;不写照插件自己写的,都没写是 popup
  popup          tmux 弹窗(不在 tmux 里就在当前终端前台跑)
  split          开在 agent 那格右边
  window         那个 tmux 会话里的新窗口
  --client       弹窗弹在哪台 tmux 客户端上",
                    "\
A plugin = a directory + an aca-plugin.toml (commands, event hooks, timers, panes, settings, a directory and an MCP server
it brings to agents) that calls back into aca's command line. How to write one: docs/plugins.md.
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
                brief: Tr::new("agent 的 hook 调的,不是给人用的", "called by agent hooks, not meant for people"),
                usage: &[Tr::new("hook <claude | codex | 工具名>", "hook <claude | codex | TOOL-NAME>")],
                more: Tr::new(
                    "  claude    Claude Code 的 hook
  codex     Codex 的 hook
  工具名    自己登记的声明式工具的 hook

一个事件调一次,事件的 JSON 在标准输入里;是哪个会话看环境里的 ACA_SESSION_ID。",
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

/// 不是命令的几页
pub const TOPICS: &[(&str, Tr)] =
    &[("keys", Tr::new("界面里的快捷键", "interface keys")), ("config", Tr::new("config.toml 怎么写", "writing config.toml"))];

const CONFIG: &str = "\
~/.config/aca/config.toml —— 都是可选的,一个字不写也能用(claude / codex 用各自默认的配置目录)。
改完马上生效,不用重启什么;写坏了的话命令行会提醒一句,并且当成没写。

账号 = 起 agent 时多带的那几样,用它自己的词写一处(aca new 工具:账号)
  [accounts.claude]
  work = \"~/.claude-work\"                                     # 只换配置目录:写一个字符串
  bedrock = { env = { CLAUDE_CODE_USE_BEDROCK = \"1\" } }        # 不换目录,换认证方式
  [accounts.codex]
  team = { home = \"~/.codex2\", args = [\"--profile\", \"team\"] }  # 目录之外还要带参数
  api = { env_file = \"~/.secrets/openai.env\" }                # key 放在文件里,每次启动现读(aca 不存)
  没登记的目录不用写在这:aca new codex:~/.codex-try

工具
  [tools.claude]                   # 内置的三家(claude / codex / shell)的默认值
  account = \"work\"                 # 没写账号时用哪个
  args = [\"--dangerously-skip-permissions\"]                   # 每次启动都带的参数
  env = { ANTHROPIC_BASE_URL = \"https://…\" }
  mod = true                       # 起 Claude 时带上 aca 的 mod:订阅额度不靠状态栏(不写:不带;界面 ^S 的 m 也能开关)
  [tools.codex]
  cmd = [\"~/bin/codex-wrapper.sh\"] # 自己包了一层脚本的
  hook_trust = \"bypass\"            # 不让 Codex 问「Hooks need review」(不写:每个账号问一次)
  [tools.ccx]                      # 自己起名的工具:aca new ccx
  kind = \"claude\"                  # 按内置的哪一家起、接 hook
  args = [\"--model\", \"opus\"]
  [tools.aider]                    # 不写 kind:声明式工具,别的 agent 不写代码就能接
  resume = [\"--session\", \"{conv}\"]
  busy = ['Working\\.\\.\\.']         # 屏幕上出现就算在跑(正则);ask 同理

会话之间递话(aca help msg)
  [inbox]
  notify = [\"done\", \"failed\"]     # worker 在哪些事上通知派它的那个(all = 都要;不写 = 都不通知)

启动时的那几道关
  [trust]
  folder = true                    # 替你答「信不信任这个目录」(Claude、Codex 头一回进一个目录都问;不写:不替你答,
                                   # 会话停在那等你;界面 ^S 的 f 也能开关)。那个目录自带的配置从此不问就能跑

闲着的会话(aca help stop;写错了只这一节当没写)
  [idle]
  stop_after = \"3h\"                # 空闲这么久就停掉,列表里标 ☾(30m、3h、1d、1h30m;不写 = 不自动停)。aca 界面开着时每 5 分钟看一次;
                                   # 这一轮没跑完的、跑完没看的、在等你的、有人连着的、有后台任务的、输入框里有没发的字的、
                                   # 派出去的会话还开着的、给自己定了还没到点的定时的不停(aca help stop)。⏎ 接着原来的对话再起

aca 绑在 tmux 上的键(不用前缀,只在 aca 的会话里拦;不写进你的 tmux.conf)
  [keys]
  next = \"C-M-n\"                   # 进下一个在等你的会话(aca help next)。不写就是它;\"\" = 不绑;aca tmux key next <键> 也是改这里

登录(账号页的 ⏎、aca run … login:不替你开默认浏览器,链接弹出来挑;写错了只这一节当没写)
  [login]
  browsers = [\"Firefox\", { name = \"Chrome 工作\", cmd = [\"google-chrome\", \"--profile-directory=Profile 1\"] }]
                                   # 不写:自动找(Mac 看 .app,Linux 看 .desktop);写了就只列这些。
                                   # 字符串:Mac 上是 App 的名字,Linux 上是命令;表:名字 + 命令,链接接在最后(写 {url} 就换在那)
  remote = \"auto\"                  # 远程(不列浏览器,换成在别的设备上登得完的):auto 看 SSH、Linux 有没有图形界面;yes / no 定死

界面
  [ui]
  lang = \"zh\"                      # 说哪种话:en(默认)/ zh;环境变量 ACA_LANG 压过它
  menu = [\"work\", \"team\"]          # n 的工具菜单里谁排前面
  hide = [\"official\"]              # 藏掉不常用的
  [mcps.web]                       # MCP 目录:M 能挂到项目(除了 description、scope,别的键原样写进 .mcp.json);
  command = \"npx\"                  # gateway 的设置 servers 里点了名的经 gateway 给所有 agent(aca help plugin)
  args = [\"-y\", \"web-mcp\"]
  description = \"搜网页\"
  scope = \"session\"                # 在 gateway 里每个会话一份(有状态的,比如浏览器);不写 = 大家共用一份
  [theme] name · [ime] auto · [new_group] default_path · [hooks] url / token
                                   # 这几样在界面的 ^S 设置页里改,不用手写

哪些名字现在能用:aca names。账号的每一项、不串号的规矩:README 的「账号」。";

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
  [tools.claude]                   # defaults for the three built-ins (claude / codex / shell)
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
  stop_after = \"3h\"                # stop sessions idle this long, drawn ☾ in the list (30m, 3h, 1d, 1h30m; unset = never).
                                   # Checked every 5 minutes while the aca interface is open; never mid-turn, finished-unread,
                                   # waiting for you, attached, with background tasks or unsent text, with dispatched
                                   # sessions still open, or with a schedule of its own not yet due (aca help stop). ⏎ resumes

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

Which names work now: aca names. Every account option and the rules that keep accounts apart: \"Accounts\" in the README.";

/// 在终端里占几格:中日韩的字和全角标点两格,别的一格(`…` `·` `→` 这些在多数终端里是一格)
fn width(s: &str) -> usize {
    let wide = |c: char| matches!(c as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6);
    s.chars().map(|c| if wide(c) { 2 } else { 1 }).sum()
}

pub fn find(name: &str) -> Option<&'static Cmd> {
    GROUPS.iter().flat_map(|g| g.1.iter()).find(|c| c.names.contains(&name))
}

/// 敲错了的名字像哪条命令:它是某个名字的开头,或者只差一两个字母
pub fn guess(name: &str) -> Option<&'static str> {
    let names = || GROUPS.iter().flat_map(|g| g.1.iter()).flat_map(|c| c.names.iter().copied());
    let starts: Vec<&str> = names().filter(|n| !name.is_empty() && n.starts_with(name)).collect();
    if let [one] = starts.as_slice() {
        return Some(one);
    }
    // 短的名字差两个字母就是另一个词了(nope 不是 move)
    let far = if name.chars().count() < 5 { 1 } else { 2 };
    names().map(|n| (distance(name, n), n)).filter(|(d, _)| *d <= far && *d < name.chars().count()).min().map(|x| x.1)
}

/// 两个词差几个字母(增、删、改各算一个)
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

/// 参数给错时打在用法前面的;第二种用法起缩进到和它一样宽
pub const USAGE: Tr = Tr::new("用法:", "usage: ");

/// 一条命令的用法,一行一种(参数给错时也打这个)
pub fn usage(name: &str) -> Option<String> {
    let c = find(name)?;
    // 几个名字共用一项的:只打这个名字的那几行
    let own: Vec<&Tr> = c.usage.iter().filter(|u| u.split_whitespace().next() == Some(name)).collect();
    let lines: Vec<String> = if own.is_empty() {
        c.usage.iter().map(|u| format!("aca {u}")).collect()
    } else {
        own.iter().map(|u| format!("aca {u}")).collect()
    };
    Some(lines.join(&format!("\n{}", " ".repeat(width(&USAGE)))))
}

/// 总览
pub fn overview() -> String {
    let pad = GROUPS.iter().flat_map(|g| g.1.iter()).map(|c| width(&c.short)).max().unwrap_or(0) + 3;
    let row = |left: &str, right: &str| format!("  {left}{}{right}\n", " ".repeat(pad.saturating_sub(width(left))));
    let mut out = tr!(
        "aca — 一屏管住所有 agent 会话(Claude Code / Codex …)。后端是 tmux,tmux 会话名就是标题\n\n",
        "aca — every agent session (Claude Code / Codex …) on one screen, on tmux; session name = title\n\n"
    );
    out += &row(
        "aca",
        &tr!(
            "打开界面:列表 + 高亮会话的实时画面,回车进去(里面按 ? 看快捷键)",
            "the interface: list + live view; Enter goes in, ? shows keys"
        ),
    );
    out += &row(
        &tr!("aca help <命令>", "aca help <COMMAND>"),
        &tr!("一条命令的细节(aca <命令> --help 也行)", "one command in detail (aca <COMMAND> --help too)"),
    );
    for (title, cmds) in GROUPS {
        out += &format!("\n{title}\n");
        for c in *cmds {
            out += &row(&c.short, &c.brief);
        }
    }
    out += &tr!(
        "\n<id> 可以只写开头几位,或者写标题。ls / show / new / adopt / group ls 加 --json 给程序读。\n",
        "\n<id>: first few characters, or the title. --json for programs: ls / show / new / adopt / group ls\n"
    );
    out += &tr!(
        "在终端里打出来的是排好、上了色的(配色跟界面的主题,设了 NO_COLOR 不上色);接到管道里是不带颜色的原样。\n",
        "In a terminal: laid out, in the interface's colors (NO_COLOR: none); piped: plain.\n"
    );
    let topics: Vec<String> = TOPICS.iter().map(|(n, what)| tr!("aca help {n}({what})", "aca help {n} ({what})")).collect();
    out += &tr!("还有:{} · aca version", "Also: {} · aca version", topics.join(" · "));
    out
}

/// `aca help <名字>`:一条命令的细节,或者那几页;没有这个名字 = None
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

/// 参数里是不是在求助:`--` 前面有 `-h` / `--help`。跟在要带一段话的参数后面的不算(`-m --help` 是要发的话)
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
                // 100 列的终端里不折行
                assert!(o.lines().all(|l| width(l) <= 100), "{l:?}:{:#?}", o.lines().filter(|l| width(l) > 100).collect::<Vec<_>>());
                for c in GROUPS.iter().flat_map(|g| g.1.iter()) {
                    assert_eq!(o.matches(&format!("\n  {} ", &*c.short)).count(), 1, "{l:?}:{}", &*c.short);
                    // 写法里的第一个词就是名字;每种用法都以自己的某个名字开头
                    assert_eq!(c.short.split_whitespace().next(), Some(c.names[0]));
                    assert!(c.usage.iter().all(|u| c.names.contains(&u.split_whitespace().next().unwrap())), "{:?}", c.names);
                }
                // 说明那一列是对齐的(中文按两格算):每行「  写法」后面空到同一格
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
        // 共用一项的两个名字都找得到同一页;用法只打自己的那一行
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

    /// 两种话的细节页排得一样:参数表的第一行和后面几行一样缩进着,没有哪行意外地缩进一格
    #[test]
    fn detail_pages_are_indented_the_same_way_in_both_languages() {
        for l in [Lang::Zh, Lang::En] {
            with(l, || {
                for c in GROUPS.iter().flat_map(|g| g.1.iter()) {
                    let page = of(c.names[0]).unwrap();
                    assert!(page.lines().all(|l| l.starts_with("  ") || !l.starts_with(' ')), "{l:?} {}", c.names[0]);
                    // 两边都有、或者都没有补充的
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
        // 开头对得上好几个的、差太远的:不猜
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
