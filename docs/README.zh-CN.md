# aca 文档

[English](README.md) | 简体中文

aca 的所有文档,按你要做的事分类。总体介绍和快速上手见 [README](../README.zh-CN.md)。

## 从这里开始

- [上手指南](onboarding.zh-CN.md):最初的十五分钟
- [安装](install.zh-CN.md):每种装法、安装脚本的参数、升级、可选的工具、aca 的文件放在哪里、卸载

## 使用指南

- [同时盯很多会话](many-sessions.zh-CN.md):知道哪个会话在等你,按一个键就过去
- [不进会话也能看](look-without-going-in.zh-CN.md):在列表里直接看会话的输出、改动和对话记录
- [多个账号,额度用完了怎么办](several-accounts.zh-CN.md):用你想要的账号启动会话,一个账号的额度用完时换另一个继续
- [用命令和脚本驱动会话](scripting.zh-CN.md):用命令和脚本操作会话,以及让一个 agent 把任务派给另一个
- [用量、花费和额度](usage-and-cost.zh-CN.md):你的用量折算是多少钱,订阅额度用了多少

## 参考

- [按键](keys.zh-CN.md):列表里、会话里面、任何 tmux 会话里的按键,以及哪些键可以改
- [常用命令](commands.zh-CN.md):最常用的命令
- [配置](configuration.zh-CN.md):`config.toml` 的每一个配置节
- [账号](accounts.zh-CN.md):登记账号,以及接入 Claude 的状态栏来查看额度
- [不是用 aca 启动的会话](adopt.zh-CN.md):接管正在运行的会话,从 agent-deck 导入
- [插件](plugins.zh-CN.md):使用和编写插件
- [jobhook](https://github.com/cheny-00/jobhook):远程任务插件(`aca.remote`)背后的小型服务端,是一个单独的项目
- `aca help`、`aca help <命令>`、`aca help keys`、`aca help config`:完整的参考,内容始终和你安装的版本一致

## 参与开发

- [开发](development.zh-CN.md):代码的结构、运行测试、隔离手动测试、发布新版本
