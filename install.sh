#!/bin/sh
# 装 / 升级 aca。只做一件事:把 aca 这一个程序放到 <prefix>/bin/aca。
# 别的都不装(tmux、Rust、fzf、yazi、lazygit … 缺了只说一声),不改 shell 的配置,不碰你的 tmux.conf。
#
#   curl -fsSL https://raw.githubusercontent.com/cheny-00/aca/main/install.sh | sh
#                                    下载编好的(macOS / Linux,arm64 / x86_64);参数接在 sh -s -- 后面
#   ./install.sh                     在仓库里:从这份源码编(要 Rust 1.89+),装或者升级
#
# 参数见下面的 usage。说哪种话和 aca 自己一样:ACA_LANG > config.toml 的 [ui] lang > 英文。
set -eu

repo=${ACA_REPO:-cheny-00/aca}
# 测试、镜像用:release 从哪下、源码从哪克隆
releases=${ACA_RELEASES:-https://github.com/$repo/releases}
source_url=${ACA_SOURCE:-https://github.com/$repo}

prefix=${PREFIX:-$HOME/.local}
setup=yes
statusline=
version=
from_source=no

lang=${ACA_LANG:-}
if [ -z "$lang" ] && [ -z "${ACA_HOME:-}" ] && [ -f "$HOME/.config/aca/config.toml" ]; then
  lang=$(sed -n '/^\[ui\]/,/^\[/s/^[[:space:]]*lang[[:space:]]*=[[:space:]]*"\([a-zA-Z]*\).*/\1/p' "$HOME/.config/aca/config.toml" | head -n 1)
fi
case $lang in zh*) zh=yes ;; *) zh=no ;; esac
# say <中文> <english>
say() { if [ "$zh" = yes ]; then printf '%s\n' "$1"; else printf '%s\n' "$2"; fi; }
die() { say "$1" "$2" >&2; exit 1; }

usage() {
  if [ "$zh" = yes ]; then
    cat <<EOF
装 / 升级 aca:只把 aca 这一个程序放到 <prefix>/bin/aca,别的都不装、不改。

  curl -fsSL https://raw.githubusercontent.com/$repo/main/install.sh | sh
                         下载编好的(macOS / Linux,arm64 / x86_64)
  curl -fsSL … | sh -s -- --version v0.1.0
                         带参数:接在 sh -s -- 后面
  ./install.sh           在仓库里:从这份源码编(要 Rust 1.89+)

  --prefix <目录>        装到 <目录>/bin/aca(默认 ~/.local,也认环境变量 PREFIX)
  --version <tag>        装哪一版(默认最新的 release);在仓库里跑时不管用
  --from-source          不下载,用 cargo 从源码编(在仓库里跑本来就是这个)
  --statusline <接法>    装完不打开设置那张表,直接定 Claude 的状态栏怎么接:aca / wrap / off
                         (意思见 aca help setup)
  --no-setup             只装程序,不打开设置,也不提下一步
EOF
  else
    cat <<EOF
Install or upgrade aca: puts the single aca binary at <prefix>/bin/aca. Installs and changes nothing else.

  curl -fsSL https://raw.githubusercontent.com/$repo/main/install.sh | sh
                         download a prebuilt binary (macOS / Linux, arm64 / x86_64)
  curl -fsSL … | sh -s -- --version v0.1.0
                         with options: put them after sh -s --
  ./install.sh           in a checkout: build from this source (needs Rust 1.89+)

  --prefix <dir>         install to <dir>/bin/aca (default ~/.local; the PREFIX variable works too)
  --version <tag>        which release (default: the latest); ignored in a checkout
  --from-source          don't download, build from source with cargo (a checkout always does)
  --statusline <how>     don't open the setup screen; set how Claude's status bar is connected
                         right away: aca / wrap / off (explained in aca help setup)
  --no-setup             only install the binary; don't open setup or print next steps
EOF
  fi
}

while [ $# -gt 0 ]; do
  case $1 in
    --prefix)
      [ $# -ge 2 ] || die "--prefix 后面要跟目录" "--prefix needs a directory"
      prefix=$2; shift 2 ;;
    --version)
      [ $# -ge 2 ] || die "--version 后面要跟 tag(像 v0.1.0)" "--version needs a tag (like v0.1.0)"
      version=$2; shift 2 ;;
    --statusline)
      [ $# -ge 2 ] || die "--statusline 后面要跟 aca、wrap 或 off" "--statusline needs aca, wrap or off"
      statusline=$2; shift 2 ;;
    --from-source) from_source=yes; shift ;;
    --no-setup) setup=no; shift ;;
    -h | --help) usage; exit 0 ;;
    *) say "不认 $1(--help 看用法)" "unknown option $1 (see --help)" >&2; exit 2 ;;
  esac
done

tmp=
cleanup() { [ -z "$tmp" ] || rm -rf "$tmp"; }
trap cleanup EXIT
trap 'exit 130' INT TERM

need_cargo() {
  command -v cargo >/dev/null 2>&1 ||
    die "从源码编要先有 Rust(1.89 以上):https://rustup.rs —— 这个脚本不替你装。" \
      "Building from source needs Rust (1.89+): https://rustup.rs — this script won't install it for you."
}

# fetch <地址> <存成哪个文件>
fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 2 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    die "下载要有 curl 或者 wget。" "Downloading needs curl or wget."
  fi
}

# 编好的是哪一个:<架构>-<系统>,和 release 里的文件名对着
target() {
  os=$(uname -s)
  arch=$(uname -m)
  case $os in
    Darwin)
      os=apple-darwin
      # Rosetta 里的 shell 报 x86_64:机器是 Apple 芯片就装 arm64 的
      [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ] && arch=arm64
      ;;
    Linux) os=unknown-linux-gnu ;;
    *) return 1 ;;
  esac
  case $arch in
    arm64 | aarch64) arch=aarch64 ;;
    x86_64 | amd64) arch=x86_64 ;;
    *) return 1 ;;
  esac
  echo "$arch-$os"
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# 在仓库里跑(./install.sh)就从这份源码编;管道里跑(curl … | sh)没有这个文件,下载编好的
here=
if [ -f "$0" ]; then
  d=$(cd "$(dirname "$0")" && pwd)
  [ -f "$d/crates/aca/Cargo.toml" ] && here=$d
fi

if [ -n "$here" ]; then
  need_cargo
  cd "$here"
  cargo build --release --locked -p aca
  built=${CARGO_TARGET_DIR:-target}/release/aca
elif [ "$from_source" = yes ]; then
  need_cargo
  tmp=$(mktemp -d)
  say "从源码编,要几分钟。先编在临时目录里;cargo 最后会提醒把那个目录加进 PATH,不用管它。" \
    "Building from source; this takes a few minutes. It builds in a temporary directory first, so ignore cargo's closing note about adding that directory to PATH."
  # --root 指到临时目录:cargo 自己记账的那两个文件不落在你的 prefix 里
  if [ -n "$version" ]; then
    cargo install --git "$source_url" --tag "$version" --locked --root "$tmp" aca
  else
    cargo install --git "$source_url" --locked --root "$tmp" aca
  fi
  built=$tmp/bin/aca
else
  t=$(target) ||
    die "没有给 $(uname -sm) 编好的。装了 Rust(1.89 以上)可以从源码编:加上 --from-source" \
      "No prebuilt binary for $(uname -sm). With Rust (1.89+) you can build from source: add --from-source"
  file=aca-$t.tar.gz
  if [ -n "$version" ]; then from=$releases/download/$version; else from=$releases/latest/download; fi
  tmp=$(mktemp -d)
  say "下载 $from/$file" "Downloading $from/$file"
  fetch "$from/$file" "$tmp/$file" ||
    die "没下到。这一版可能没有给 $t 编好的,或者网络不通;装了 Rust(1.89 以上)可以从源码编:加上 --from-source" \
      "Download failed. This release may have no build for $t, or the network is down; with Rust (1.89+) you can build from source: add --from-source"
  fetch "$from/$file.sha256" "$tmp/$file.sha256" ||
    die "没下到校验和($file.sha256),不装。" "Couldn't download the checksum ($file.sha256); not installing."
  want=$(cut -d' ' -f1 <"$tmp/$file.sha256")
  got=$(sha256 "$tmp/$file")
  if [ -z "$got" ]; then
    say "提醒:没有 sha256sum / shasum,没核对校验和。" "Note: no sha256sum / shasum here, the checksum wasn't verified." >&2
  elif [ "$got" != "$want" ]; then
    die "校验和对不上,不装(要的是 $want,下到的是 $got)。" "Checksum mismatch, not installing (wanted $want, got $got)."
  fi
  tar -xzf "$tmp/$file" -C "$tmp"
  built=$tmp/aca
fi

dest=$prefix/bin/aca
first=yes
[ -e "$dest" ] && first=no
mkdir -p "$prefix/bin"
# 换名字装:各会话的 hook、状态栏随时在跑这个程序,开着的界面也是它,不能直接往原文件上拷
cp "$built" "$dest.new.$$"
chmod 755 "$dest.new.$$"
mv -f "$dest.new.$$" "$dest"
v=$("$dest" --version)
say "装好了 $v:$dest" "Installed $v at $dest"

# 装过 aca 的 tmux 设置(aca tmux install)的:那份照新的重写,开着的 tmux 也换上
if [ -e "$HOME/.config/aca/tmux.conf" ] && [ -z "${ACA_HOME:-}" ]; then
  "$dest" tmux install </dev/null >/dev/null &&
    say "aca 的 tmux 设置照新的重写了(aca help tmux)" "Rewrote aca's tmux settings to match (aca help tmux)"
fi

# 下面只说、不装。没有 tmux 也装得上,但一个会话都起不了
if command -v tmux >/dev/null 2>&1; then
  set -- $(tmux -V | sed 's/[^0-9]*\([0-9][0-9]*\)\.\([0-9][0-9]*\).*/\1 \2/')
  if [ "${1:-0}" -lt 3 ] || { [ "${1:-0}" -eq 3 ] && [ "${2:-0}" -lt 3 ]; }; then
    say "提醒:tmux 是 $(tmux -V),aca 要 3.3 以上。" "Note: tmux is $(tmux -V); aca needs 3.3 or newer."
  fi
else
  say "提醒:没找到 tmux(要 3.3 以上)。aca 的会话都开在 tmux 里,装上它再用。" \
    "Note: tmux not found (3.3+ needed). Every aca session lives in tmux; install it before using aca."
fi
if ! command -v claude >/dev/null 2>&1 && ! command -v codex >/dev/null 2>&1; then
  say "提醒:没找到 claude,也没找到 codex。aca 管的是它们的会话,至少装一个。" \
    "Note: neither claude nor codex found. aca manages their sessions; install at least one."
fi
missing=
for tool in git fzf jq yazi lazygit zoxide; do
  command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
  say "可选的工具,这台机器上没有:${missing}。不装也能用,只是各少一个功能;哪个管什么见 README 的「可选的工具」。" \
    "Optional tools not found:$missing. aca works without them; each one only adds a feature. See \"Optional tools\" in the README."
fi
case ":$PATH:" in
  *":$prefix/bin:"*) ;;
  *) say "提醒:$prefix/bin 不在 PATH 里,加上才能直接敲 aca。" "Note: $prefix/bin isn't on your PATH; add it to run aca by name." ;;
esac

[ "$setup" = yes ] || exit 0
if [ -n "$statusline" ]; then
  echo
  "$dest" setup --statusline "$statusline" </dev/null
elif [ "$first" = no ]; then
  say "升级完了,你的设置没动。要看或者改账号、Claude 的状态栏:aca setup" \
    "Upgraded; your settings are untouched. To review accounts or Claude's status bar: aca setup"
elif [ -t 0 ] && [ -t 1 ]; then
  echo
  "$dest" setup
else
  # 管道里跑的(curl … | sh):这时的标准输入是脚本本身,表开不了,只说下一步
  say "下一步:
  aca setup    可以跳过。登记多出来的账号、选要不要让 aca 显示 Claude 的订阅额度;按 Esc 什么都不改
  aca          打开界面(按 n 新建会话,? 看快捷键)" \
    "Next:
  aca setup    optional: register extra accounts and choose whether aca shows Claude's usage limits; Esc changes nothing
  aca          open the interface (n starts a session, ? lists the keys)"
fi
