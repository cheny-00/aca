# 配置

[English](configuration.md) | 简体中文

aca 的配置文件里能设置什么,以及每一部分的详细说明在哪里。

所有配置都是可选的。没有配置文件 aca 也能正常使用。

配置写在 `~/.config/aca/config.toml` 里,修改后立即生效,无需重启。文件不是合法的 TOML 时,aca 会提示一次,
然后按空配置继续运行。一般不需要手动编辑:大多数设置都能在设置页(`Ctrl+S`)里修改,并自动写入这个文件;
在设置页按 `e` 会用编辑器打开它。

下面是最常改的几项:

```toml
[tools.claude]
account = "work"                          # aca new claude 没有指定账号时使用的账号
args = ["--model", "opus"]                # 每次启动都带上的参数(也可以在账号页按 p 修改)

[tools.codex]
hook_trust = "bypass"                     # 不再询问「Hooks need review」(见上手指南)

[trust]
folder = true                             # 替你回答「是否信任这个目录」

[idle]
stop_after = "3h"                         # 界面打开时,停止空闲超过这么久的会话(⏎ 重新启动并继续原来的对话);
                                          # 空闲的 agent 也会一直占用 CPU
remove_after = "30d"                      # 删除已停止超过这么久的会话(移到回收站,1 天内可用 ^Z / b 恢复);
                                          # 写 "never" 或不写 = 不删除;置顶的、有便签的不会删除

[keys]
next = "M-n"                              # 「进入下一个待处理的会话」改用 Alt+N,而不是 Ctrl+Alt+N

[login]
browsers = ["Firefox", "Safari"]          # 登录链接可以用哪些浏览器打开(不写:列出已安装的浏览器)

[ui]
lang = "zh"                               # "en" 或 "zh"
menu = ["work", "personal"]               # n 菜单里哪些排在前面

[tools.aider]                             # 接入其他 agent:只需声明,不用写代码
resume = ["--session", "{conv}"]
busy = ['Working\.\.\.']
```

全部的配置节,以及去哪里看详细说明:

| 配置节 | 用来做什么 | 详细说明 |
|---|---|---|
| `[accounts.claude]` `[accounts.codex]` | 额外的账号 | [账号](accounts.zh-CN.md) |
| `[tools.<名字>]` | 内置的 Claude、Codex、shell 的默认设置(账号、参数、环境变量、外层包装的启动命令);自己命名的变体;不用写代码就能接入的其他 agent | `aca help config` |
| `[trust]` | 替你回答「是否信任这个目录」 | [上手指南](onboarding.zh-CN.md#第一次会被问到的两个问题) |
| `[idle]` | 把空闲了一段时间的会话停掉 | `aca help stop` |
| `[inbox]` | 派出去的会话在哪些情况下通知派它的会话:完成、失败等 | `aca help msg` |
| `[keys]` | aca 绑定在 tmux 上、进入下一个待处理会话的键 | [按键](keys.zh-CN.md) |
| `[login]` | 登录链接可以用哪些浏览器打开;通过 SSH 连接时改为在其他设备上完成登录 | `aca help config` |
| `[ui]` | 语言;`n` 菜单的顺序,以及要隐藏的项 | `aca help config` |
| `[mcps.<名字>]` | 你的 MCP server 目录:用 `M` 挂到某个项目上,或者通过 gateway 插件提供给所有 agent | [docs/plugins.zh-CN.md](plugins.zh-CN.md) |
| `[theme]` `[ime]` `[new_group]` | 主题、自动切换输入法(只有 macOS)、新分组的默认目录 | 在设置页(`Ctrl+S`)里改 |

- `aca help config` 列出每一项配置并附有示例;`aca names` 列出现在能用的账号名和工具名。
- 插件的设置单独放在 `~/.config/aca/plugins.toml` 里,在插件页(`P`)里改。见 [docs/plugins.zh-CN.md](plugins.zh-CN.md)。
- 环境变量在当前终端里优先于配置文件:`ACA_LANG`(`en` / `zh`)、`ACA_THEME`、`ACA_IME`。
  `ACA_HOME` 把 aca 的所有文件放到另一个目录,适合用来测试。
