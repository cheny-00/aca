//! 跟随(`aca ui --client … --follow`):界面开在 tmux 弹窗里、只占屏幕一边时,高亮挪到哪,
//! 这台客户端就切到哪个会话。弹窗是挂在客户端上的,切了会话它还在,旁边露出来的就是高亮的那个。
//!
//! - ⏎ 留在那个会话(和不跟随时一样:切过去、弹窗关、算看过)
//! - Esc 回到打开时停着的那一格
//! - Ctrl+\(开弹窗的那个键)/ qq 关掉,人留在旁边现在露出来的那个,它算看过
//!
//! 高亮停稳了才切:滑动途中、正在打过滤词时不切;停在分组上、没在跑的会话上,旁边不动。
//! 路过不算看过:切过去 tmux 就记它「连进去过」,核心那边的「跑完没看」跟着没了,所以路过的记成未读(见 reads.rs)。
//! 预览默认不画(旁边就是真的),`v` 换的只管这一次,不动手机 / 桌面各自记着的布局。
//!
//! 这里只管「该不该切、切到哪」;发 tmux 命令的是 `data::show`,接进界面的几处在 ui.rs(`follow_step` / `follow_done`)。

use std::sync::atomic::{AtomicBool, Ordering};

use crate::ui::Preview;

/// 启动时带了 `--follow`(^L 重启时原样带上)
static ASKED: AtomicBool = AtomicBool::new(false);

pub fn ask() {
    ASKED.store(true, Ordering::Relaxed);
}

pub fn asked() -> bool {
    ASKED.load(Ordering::Relaxed)
}

/// 客户端停着 / 要去的地方
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    /// 列表里那一行的键(`Session::name`);打开时高亮没落在哪一行上就是空的
    pub row: String,
    /// tmux 的会话号(`$3`)
    pub session: String,
    /// 落在哪一格(`%12`);空 = 那个会话现在的那一格
    pub pane: String,
}

impl Place {
    fn same_place(&self, o: &Place) -> bool {
        (&self.session, &self.pane) == (&o.session, &o.pane)
    }
}

pub struct Follow {
    /// 打开时:高亮的那一行,和客户端真正停着的会话、窗格(那一行自己的窗格可能是另一格,回来时落回原来这格)
    home: Place,
    /// 客户端现在被带到了哪
    at: Place,
    /// 按了 Esc:下一步回 home
    back: bool,
    /// 这一次用的布局
    pub layout: Preview,
}

impl Follow {
    pub fn new(home: Place) -> Self {
        Follow { at: home.clone(), home, back: false, layout: Preview::Off }
    }

    /// 高亮停稳在 want 上(None = 还没停稳,或者停在切不过去的行上)。要切就给出切到哪,已经在那就不给
    pub fn step(&mut self, want: Option<Place>) -> Option<Place> {
        let to = match want {
            _ if self.back => self.home.clone(),
            // 挪回打开时那一行:落回原来那一格,不是那一行自己的窗格
            Some(w) if w.row == self.home.row => self.home.clone(),
            Some(w) => w,
            None => return None,
        };
        if to.same_place(&self.at) {
            return None;
        }
        self.at = to.clone();
        Some(to)
    }

    /// Esc:回到打开时停着的地方
    pub fn back(&mut self) {
        self.back = true;
    }

    /// 是不是打开时停着的那个会话
    pub fn is_home(&self, session: &str) -> bool {
        self.home.session == session
    }

    /// 关掉时人留在了别的会话:它的会话号
    pub fn stayed(&self) -> Option<&str> {
        (!self.is_home(&self.at.session)).then_some(self.at.session.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(row: &str, session: &str, pane: &str) -> Place {
        Place { row: row.into(), session: session.into(), pane: pane.into() }
    }

    #[test]
    fn opening_moves_nothing_and_each_row_is_switched_to_once() {
        // 打开时停在会话 a 的一个 shell 格(%7)上,高亮落在 a 那一行(它自己的窗格是 agent 的 %1)
        let mut f = Follow::new(spot("a", "$1", "%7"));
        assert_eq!(f.step(Some(spot("a", "$1", "%1"))), None, "刚打开:不动");
        assert_eq!(f.step(None), None, "滑动途中 / 停在分组上:不动");
        assert_eq!(f.step(Some(spot("b", "$2", "%3"))), Some(spot("b", "$2", "%3")));
        assert_eq!(f.step(Some(spot("b", "$2", "%3"))), None, "已经在那:不再发命令");
        // 同一个会话里另一格也要切
        assert_eq!(f.step(Some(spot("b\u{1}%4", "$2", "%4"))), Some(spot("b\u{1}%4", "$2", "%4")));
        // 挪回打开时那一行:落回原来那格(%7),不是 agent 的 %1
        assert_eq!(f.step(Some(spot("a", "$1", "%1"))), Some(spot("a", "$1", "%7")));
        assert_eq!(f.stayed(), None);
    }

    #[test]
    fn esc_goes_back_to_where_it_opened_whatever_is_highlighted() {
        let mut f = Follow::new(spot("a", "$1", "%7"));
        f.step(Some(spot("b", "$2", "%3")));
        assert_eq!(f.stayed(), Some("$2"));
        f.back();
        // 高亮还在 b 上、甚至还在滑:照样回去
        assert_eq!(f.step(Some(spot("b", "$2", "%3"))), Some(spot("a", "$1", "%7")));
        assert_eq!((f.step(None), f.stayed()), (None, None));
        // 没挪过就按 Esc:本来就在,不发命令
        let mut f = Follow::new(spot("a", "$1", "%7"));
        f.back();
        assert_eq!(f.step(None), None);
    }

    #[test]
    fn the_preview_is_off_until_asked_for() {
        assert_eq!(Follow::new(Place::default()).layout, Preview::Off);
    }
}
