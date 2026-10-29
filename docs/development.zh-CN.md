# 开发

[English](development.md) | 简体中文

怎样在这个仓库的代码上编译、测试和发布 aca。

```sh
cargo test                               # 单元测试和端到端测试(需要 tmux;测试会自己启动一个隔离的 tmux 服务)
cargo clippy --all-targets               # 静态检查;别对整个仓库跑 cargo fmt,代码还没统一格式化过
./install.sh                             # 编译当前这份代码并安装
```

代码分成三个 crate:`crates/aca-core`(会话后端,是一个库)、`crates/aca-tui`(界面,只依赖 core)、
`crates/aca`(命令行,也是界面的入口)。自带的插件在 `plugins/` 下,会被编译进程序里。
每条命令的用法和帮助文字都写在 `crates/aca/src/help.rs` 的一张表里;
界面的按键说明在 `crates/aca-tui/src/keys.txt` 和 `keys.en.txt` 里。

**手动测试时请做好隔离。** 设置 `ACA_HOME=<一个临时目录>`,配置、会话数据库和状态就都放在那里,不会碰到你真实的数据;
设置 `ACA_TMUX_SOCKET=<一个名字>` 则使用一个单独的 tmux 服务。
`aca ui --snapshot 100x30 [按键]` 可以把界面的一帧画成文字输出,不会真的执行任何操作。
如果你在某个 agent 会话里手动启动测试用的 agent,请先清除外层会话的环境变量(所有以 `CLAUDE` 开头的):
tmux 服务会把自己启动时的环境变量传给每一个会话,其中 `CLAUDE_SECURESTORAGE_CONFIG_DIR`
会让里面启动的 Claude 使用外层账号的登录。aca 自己启动 agent 时会去掉这些变量,但手写的测试脚本不会。

发布新版本:推送一个形如 `v0.1.0` 的 tag。`.github/workflows/release.yml` 里的流程会编译出安装脚本要下载的那些文件。
