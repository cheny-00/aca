//! 弹窗和设置页的接口:一个功能的界面写在它自己的文件里,不进 ui.rs。
//!
//! - `Overlay`:一个弹窗 —— 它记着什么、按键怎么走、画哪些行。ui.rs 只管把它压进栈、把键转给最上面那层、
//!   把它给的行画进弹窗的框(桌面居中、手机从底下升起,和别的弹窗一样)。
//! - `Setting`:`^S` 设置页里的一行。
//! - `Fx`:弹窗 / 设置行要界面做的事。它们碰不到 App,只把要做的写在这里交回去,所以不开界面也能测。
//! - `kit`:画行用的零件。
//!
//! 加一个带界面的功能:
//! 1. 自己的文件里实现 `Overlay`(设置页要有一行,再实现 `Setting`),main.rs 加 `mod`
//! 2. ui.rs 加一处入口 —— 列表上的键:`on_key` 里一行 `self.open(…)`;设置页的行:`run` 里一行 `with_setting(…)`
//! 3. 要放到后台跑的活:ops.rs 的 `Job` 加一个变体,带功能自己的类型(`Job::Mcp`、`Job::Group`、`Job::Plugins`)
//! 4. help.rs / README 写上键

use ratatui::crossterm::event::KeyEvent;
use ratatui::text::{Line, Span};

use crate::ops::Job;
use crate::ui::{External, TermReq};

/// 一个弹窗
pub trait Overlay {
    /// 桌面上弹窗多宽(手机上占满)
    fn width(&self) -> u16;
    /// 里面的行;w = 一行能写多宽
    fn lines(&self, w: usize) -> Vec<Line<'static>>;
    /// 大小固定的弹窗(插件页):桌面上高度占满屏幕(留点边),换了高亮框也不跟着变;行由 `lines_in` 给
    fn fill(&self) -> bool {
        false
    }
    /// `fill` 的弹窗里面最多要几行(不算外框和底下那句话);None = 屏幕放得下多少占多少。
    /// 内容不多的页(账号页)照内容定高,不留大片空白;要自己保证挪高亮时这个数不变
    fn fill_rows(&self) -> Option<u16> {
        None
    }
    /// `fill` 的弹窗:正好 h 行(多了截掉,少了补空行)
    fn lines_in(&self, w: usize, _h: usize) -> Vec<Line<'static>> {
        self.lines(w)
    }
    /// 一个键。要界面做的事写进 fx;返回这层留不留
    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow;
    /// 后台的活做完了、从编辑器回来了:显示的值重读
    fn reload(&mut self) {}
    /// 从让出终端跑的程序(编辑器…)回来了,要接着做的写进 fx(插件页:plugins.toml 改了就同步)。只问最上面那层
    fn returned(&mut self, _fx: &mut Fx) {}
    /// 正在打字(弹窗里自己的输入框):这时一下到好几个键是粘贴,不当成代敲的丢掉
    fn typing(&self) -> bool {
        false
    }
    /// 插件的列表视图:这个请求(重读它自己、它的键跑完了要重读)归不归它
    fn takes(&self, _req: &crate::caps::Req) -> bool {
        false
    }
    /// 它自己的命令又跑了一遍:换上新的输出
    fn feed(&mut self, _ok: bool, _out: &str) {}
    /// 它的一个键跑完了:跑出来的(写在列表下面)
    fn said(&mut self, _ok: bool, _out: &str) {}
}

/// 按完键这层弹窗怎么办
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flow {
    Stay,
    Close,
}

/// 要界面做的事,都可以不填
#[derive(Default)]
pub struct Fx {
    /// 说一句:写在底栏;弹窗还开着的话也写在弹窗最下面(手机上弹窗盖着底栏)
    pub flash: Option<String>,
    /// 放到后台做(别的活还没做完就不做,底栏说「还在忙」);做完底栏说结果,开着的弹窗 `reload`
    pub job: Option<Job>,
    /// 让出终端跑别的程序(编辑器…),回来接着显示
    pub external: Option<External>,
    /// 下半屏开个终端
    pub term: Option<TermReq>,
    /// 再开一层,关了回到现在这层
    pub open: Option<Box<dyn Overlay>>,
    /// 复制这段字(手机上走 OSC 52 进手机的剪贴板),底栏说复制了什么
    pub copy: Option<String>,
    /// 跑一个插件的能力 / 视图 / 窗格(见 caps.rs):在后台跑,不占 `job`
    pub plugin: Option<crate::caps::Req>,
    /// 这层关了之后,当作在主列表上按了这个键(`:` 面板里挑的 aca 动作)
    pub key: Option<KeyEvent>,
}

impl Fx {
    pub fn say(&mut self, m: impl Into<String>) {
        self.flash = Some(m.into());
    }
}

/// `^S` 设置页里功能自己的一行:「键  名字  现在的值  ›」
pub trait Setting {
    /// 行首的键(别和设置页已有的重了:t i a n g e,功能自己的:d Discord 桥、h 远端任务、m Claude 的 mod、f 自动信任目录)
    fn key(&self) -> char;
    fn name(&self) -> &'static str;
    /// 现在的值:设置页打开、重读时各问一次,不每帧问
    fn value(&self) -> Vec<Span<'static>>;
    /// 行尾的记号:› = 进去再挑,↗ = 用编辑器打开,空 = 当场切
    fn mark(&self) -> &'static str {
        "›"
    }
    /// ⏎ / 按了它的键
    fn enter(&self, fx: &mut Fx);
}

/// 画行用的零件(样子和别的弹窗一致)
pub mod kit {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    pub use crate::ui::{button, clip, dim, esc_hint, key_style, keys_line, modal_title, spread};

    /// 高亮的那行:补满一行宽,铺上选中的底色
    pub fn picked(line: Line<'static>, w: usize, on: bool) -> Line<'static> {
        shaded(line, w, on.then(|| crate::theme::pal().sel))
    }

    /// 补满一行宽,铺上这个底色(None = 不铺)
    pub fn shaded(mut line: Line<'static>, w: usize, bg: Option<ratatui::style::Color>) -> Line<'static> {
        if let Some(bg) = bg {
            let pad = w.saturating_sub(line.width());
            line.spans.push(Span::raw(" ".repeat(pad)));
            line = line.patch_style(Style::new().bg(bg));
        }
        line
    }
}
