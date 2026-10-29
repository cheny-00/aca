# 用命令和脚本驱动会话

[English](scripting.md) | 简体中文

界面能做的事都有对应的命令。shell 脚本、定时任务或者另一个 agent 都可以启动会话、给它派任务、等它完成、
替它回答问题、读它的回复。这一页讲有哪些命令,以及怎样把它们组合起来。

## 基本命令

会话 id 只写开头几位就行,也可以直接写会话的标题。

| 命令 | 作用 |
|---|---|
| `aca new <工具[:账号]> [目录] [-t 标题] [-g 分组] [--say 话]` | 启动一个会话。打印它的 id,这是标准输出里唯一的一行。`--say` 是第一句话。`-w <分支>` 在 git worktree 里启动 |
| `aca send <id> -m <话>` | 在它的输入框里输入这句话并回车。`--wait <秒>` 先等它空闲 |
| `aca wait <id>…` | 等它完成这一轮、提出问题、出错或退出。退出码说明是哪一种 |
| `aca output <id>` | 它最后的回复 |
| `aca answer <id>` | 查看它在问什么:问题和带编号的选项,或者它想执行的命令 |
| `aca answer <id> <选项号 \| yes \| no>` | 替它回答:选那个选项,或者允许 / 拒绝权限请求 |
| `aca interrupt <id>` | 方向不对时:中断它正在运行的这一轮(相当于按 `Esc`) |
| `aca msg <id> <话>` | 把话放进它的收件箱,在它方便时送达(见[下文](#等合适的时机再送达的消息)) |
| `aca ls`、`aca show <id>` | 列表;某个会话的全部信息 |

会话在等待确认时,`aca send` 不会输入,因为回车可能会选中默认选项。这时用 `aca answer`。

### `aca wait` 的退出码

| 退出码 | 含义 |
|---|---|
| `0` | 完成了一轮 |
| `2` | 在等待确认或选择 |
| `3` | 这一轮出错 |
| `4` | 已退出或被停止 |
| `124` | `--timeout <秒>` 先到了 |

给了多个 id 时,等全部会话,返回第一个不是 `0` 的退出码;加 `--any` 则只要有一个有结果就返回。

## 一个完整的脚本

```sh
#!/bin/sh
# 把一个 bug 交给 Claude,等它查完,再让它补测试。
id=$(aca new claude ~/code/app -t fix-500 --say '登录页返回 500,查一下原因')

aca wait "$id"
case $? in
  0) aca output "$id" ;;          # 完成了一轮:打印它的回复
  2) aca answer "$id"; exit 2 ;;  # 它在提问:显示问的是什么,交给你处理
  *) aca show "$id"; exit 1 ;;    # 出错或退出了
esac

aca send "$id" -m '再给它补一个测试'
aca wait "$id" && aca output "$id"
```

脚本运行时,这个会话和其他会话一样出现在列表里:可以按 `z` 看,也可以进去。
`aca wait` 以退出码 `2` 返回时,`aca answer` 会打印类似这样的内容:

```
3f2a9c1d 正在询问:Use pytest or unittest for the new tests?
  1 pytest
  2 unittest
回答:aca answer 3f2a9c1d <选项号>
```

## 给程序读的输出

输出到终端时会排版并着色;通过管道传给其他程序时是纯文本,id 写全。给程序用的:

- `aca ls`、`aca show`、`aca new`、`aca adopt`、`aca group ls` 加 `--json`。例如找出所有等待回应的会话的 id:
  `aca ls --json | jq -r '.[] | select(.state == "ask") | .id'`
- `aca inbox <id> --json`:它收到的消息,是一个 JSON 数组,每条带发件人、内容和 `stage`(是否已送达)。
- `aca events -f`:会话发生的一切(一轮开始、结束、出错,提出问题、被回答……)以 JSON 行实时输出。
  有哪些事件、各有什么字段,见[事件](plugins.zh-CN.md#事件)。

## 等合适的时机再送达的消息

`aca send` 是立即在输入框里输入,要求会话空闲。`aca msg` 则把话放进会话的收件箱,由 aca 在合适的时候送达,
不动它的输入框。Claude 会立即取走,正在运行就并入这一轮。Codex 放进它自己的队列:空闲时几秒内开始一轮,
正在运行就等这一轮结束。

```sh
aca msg 3f2a 'API 文档改了,看 docs/api.md'
aca msg --in 1h30m 3f2a '看看 CI 过了没有'   # 过一段时间再送(最长 7 天)
aca inbox 3f2a                              # 收到了什么,送达了没有
```

定时的消息在 `aca inbox` 里显示为 `[定时 12:27]`,在列表里标题后面显示 `◷ 12:27`;送达之前,
这个会话不会因为空闲被自动停止。会话也可以给自己定一条,到时间叫醒自己。

## 一个 agent 把任务交给另一个

运行在 aca 会话里的 agent 也能用这些命令。它用 `aca new` 启动的会话就是它的子会话:在列表里挂在它下面,
而且只有它(或者你)能对子会话 `answer`、`interrupt`。在子会话里,`aca msg parent '…'` 发给启动它的那个会话。

两个自带插件让这件事更省事。用 `aca plugin install aca.core` 和 `aca plugin install aca.delegate` 安装,
或者按 `P`,在「可安装」里选:

- **`aca.core`** 给 Claude 和 Codex 一组调用这些命令的工具:`sessions`、`dispatch`、`message`、
  `wait`(每次最多等 240 秒)、`answer`、`interrupt` 和 `wakeup`(给自己定一条消息)。
  `aca plugin allow aca.core` 列出它们。
- **`aca.delegate`** 给你的 agent 一个 `/aca:delegate`(Claude 里是斜杠命令,Codex 里是 skill):
  它规划任务,通过 aca 派出 worker,检查它们做的结果,再向你汇报。派给谁是一项设置:
  `aca plugin setting aca.delegate worker codex:work`(默认 `codex`)。worker 的一轮因为限流或过载失败时,
  插件会隔一会儿替你让它继续,最多几次。

### 父会话怎样知道子会话做完了

要么它自己等,用 `aca wait <子会话>`;要么要求被通知。要通知,启动子会话时加 `--notify`,
或者在 `config.toml` 里给所有子会话统一设置:

```sh
aca new codex . -t worker --notify done,failed --say '…'
```

```toml
[inbox]
notify = ["done", "failed"]    # 还有 "exited"、"waiting";"all" 表示全部
```

这样子会话完成一轮、出错、退出或等待确认时,它自己的 hook 会往父会话的收件箱里放一条简短的消息,并唤醒父会话:
是谁、发生了什么、它最后一句话,以及查看全文的 `aca output <id>`。两样都没设置时,什么都不发,也不唤醒谁。
父会话已经用 `aca wait` 等过的子会话,它的消息会标为已送达,不会再唤醒一次。

## 更多

- [常用命令](commands.zh-CN.md):每条命令的说明
- [配置](configuration.zh-CN.md):`[inbox]` 以及 `config.toml` 的其他内容
- [插件](plugins.zh-CN.md#第一方插件):`aca.core`、`aca.delegate` 和其他自带插件
- `aca help`,再看 `aca help <命令>`:`new`、`send`、`wait`、`answer`、`interrupt`、`msg`、`inbox`
