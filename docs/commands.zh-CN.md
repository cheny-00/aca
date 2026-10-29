# 常用命令

[English](commands.md) | 简体中文

最常用的命令,每条一个例子。

界面里能做的事都有对应的命令。会话的 id 只写开头几位就行,也可以直接写会话的标题。

```sh
aca new claude ~/code/app                 # 在某个目录里启动一个 Claude 会话
aca new codex . -t "修登录" -g work       # 指定标题,并放进一个分组
aca new claude:work . --say '先读一下 README'    # 使用名为 work 的账号,并带上第一句话
aca ls                                    # 列出所有会话
aca attach 3f2a                           # 进入会话(Ctrl+Q 回来)
aca send 3f2a -m '接着做第二步'           # 在会话的输入框里输入一句话并回车
aca wait 3f2a                             # 等待它完成这一轮、提出问题或出错
aca answer 3f2a 2                         # 替它选第 2 个选项
aca interrupt 3f2a                        # 方向不对时:中断它正在运行的这一轮
aca stop 3f2a && aca start 3f2a           # 停止,再在原处启动并继续原来的对话
aca fork 3f2a -t "试试另一个方案"         # 从它当前的对话分出一个新会话
aca rename 3f2a "登录页改版"              # tmux 会话名也一起修改
aca cost                                  # 用量和花费
aca help                                  # 一屏列出所有命令;aca help <命令> 查看某个命令的详细说明
```

输出到终端时会排版并着色;通过管道传给其他程序时是纯文本。
`ls`、`show`、`new`、`adopt` 加上 `--json` 输出供程序读取的 JSON。
