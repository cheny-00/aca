# Development

English | [简体中文](development.zh-CN.md)

How to build, test and release aca from a checkout of this repository.

```sh
cargo test                               # unit tests and end-to-end tests (needs tmux; they start an isolated server)
cargo clippy --all-targets               # lints; don't run cargo fmt over the whole tree, it isn't formatted yet
./install.sh                             # build this checkout and install it
```

Three crates: `crates/aca-core` (the session backend, a library), `crates/aca-tui` (the interface, depends
only on core) and `crates/aca` (the command line, and the entry point of the interface). Bundled plugins
are in `plugins/` and are compiled into the binary. Each command's usage and help text lives in one table
in `crates/aca/src/help.rs`; the interface keys are described in `crates/aca-tui/src/keys.en.txt` and
`keys.txt`.

**Keep manual tests isolated.** Set `ACA_HOME=<a temporary directory>` to keep configuration, the session
database and state out of your real ones, and `ACA_TMUX_SOCKET=<a name>` to use a separate tmux server.
`aca ui --snapshot 100x30 [keys]` renders one frame of the interface as text without doing anything. If
you start test agents by hand from inside an agent session, clear the outer session's variables first
(everything starting with `CLAUDE`): a tmux server passes the environment it was started with to every
session, and `CLAUDE_SECURESTORAGE_CONFIG_DIR` in particular makes an inner Claude use the outer
account's login. aca strips these itself when it starts an agent; hand-written test scripts do not.

To publish a release, push a tag such as `v0.1.0`. The workflow in `.github/workflows/release.yml` builds
the binaries that the install script downloads.
