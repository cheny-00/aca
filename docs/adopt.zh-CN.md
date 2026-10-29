# 不是用 aca 启动的会话

[English](adopt.md) | 简体中文

怎样把你手动启动的会话交给 aca 管理(无需停止),以及怎样从 agent-deck 导入。

不是 aca 启动的会话(比如你手动在 tmux 里运行的 `claude`)会出现在界面的「其他 tmux 会话」里。
你可以查看、进入它们,但不能重命名、重启或分叉。要把它交给 aca 管理(**无需停止**):

```sh
aca adopt                      # 列出可以接管的会话
aca adopt my-session           # 接管其中一个(界面里按 A)
aca adopt --all
aca release <id>               # 移出 aca:只删除登记,tmux 会话和其中的进程保留
```

aca 会判断这个会话用的是哪个工具、哪个账号,以及它正在进行哪一段对话。
在第一次重启之前,接管的会话没有 aca 的 hook,所以它的状态只能靠读屏幕来判断,
也不会出现「未读」状态。运行一次 `aca restart <id>`(继续原来的对话)之后,它就和其他会话一样了。

之前用 [agent-deck](https://github.com/asheshgoplani/agent-deck)?`aca import agent-deck` 会列出它的分组和会话,
加上 `--apply` 才会实际导入。仍在 agent-deck 中运行的会话,建议用上面的接管方式。
