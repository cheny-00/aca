//! 输入法:列表里的字母键是命令,拼音这类输入法开着时字母先进了它的候选框,终端根本不发给 aca。
//! 桌面上在 aca 里就切到英文(ABC),要打字的地方(搜索、输入框、临时终端)和让出终端时(进会话、编辑器、fzf、退出)
//! 切回原来的。本来就是英文的不动,也就不用切回。
//!
//! - 只管本机的终端:手机隔着 Moshi,iPhone 的键盘管不了;从手机开的 aca 也不去动 Mac 的输入法
//! - 系统设置里「自动切换到文稿的输入源」关着时输入法全系统就一份,所以离开时一定切回去
//! - tmux 弹窗收不到焦点事件(窗格收得到):弹窗开着切到别的 App,那边也是英文,回来进会话 / 关掉时才切回

use std::sync::Mutex;

enum St {
    /// 不管(手机、测试、不是 macOS)
    Off,
    /// 在让出去的地方(打字、进会话),没占着
    Out,
    /// 在列表里:切到了英文就记着原来的,本来就是英文的是 None
    In(Option<tis::Source>),
}

static ST: Mutex<St> = Mutex::new(St::Off);

/// 开 aca 时:本机的终端里才管,管就马上切(弹窗一出来就按的键不被拼音吃掉)
pub fn start() {
    if !local() {
        return;
    }
    if let Ok(mut st) = ST.lock() {
        *st = St::Out;
    }
    english(true);
}

/// true:切到英文,记下原来的;false:切回原来的。和上次一样就什么也不做(每一轮都会调)
pub fn english(on: bool) {
    let Ok(mut st) = ST.lock() else { return };
    match (&*st, on) {
        (St::Out, true) => {
            let was = tis::Source::current().filter(|s| !s.ascii());
            let was = was.filter(|_| tis::Source::english().is_some_and(|e| e.select()));
            *st = St::In(was);
        }
        (St::In(_), false) => {
            if let St::In(Some(was)) = std::mem::replace(&mut *st, St::Out) {
                was.select();
            }
        }
        _ => {}
    }
}

/// 设置改了(设置页、config.toml 的 [ime] auto):按现在的设置重新决定管不管。
/// 要管:先记成让出去了,主循环下一帧自己切;不管了:切回原来的再撒手
pub fn reconfigure() {
    if cfg!(test) {
        return;
    }
    let want = local();
    let Ok(mut st) = ST.lock() else { return };
    match (&*st, want) {
        (St::Off, true) => *st = St::Out,
        (St::Off, false) | (_, true) => {}
        (_, false) => {
            if let St::In(Some(was)) = std::mem::replace(&mut *st, St::Off) {
                was.select();
            }
        }
    }
}

/// 设了 ACA_IME:设置页标出来(这个终端里听它的)
pub fn forced() -> bool {
    std::env::var("ACA_IME").is_ok_and(|v| v == "0" || v == "1")
}

/// 在管输入法(桌面)
pub fn active() -> bool {
    ST.lock().is_ok_and(|st| !matches!(*st, St::Off))
}

/// 崩了也切回去(正常退出、`?` 带出来的错误走的是同一条路,重复调没关系)
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        english(false);
    }
}

/// 按键的是不是本机的终端:tmux 里看这台客户端的终端类型(弹窗 = `--client` 那台),
/// 手机经 mosh 连进来的是 xterm-256color 之类;不在 tmux 里看 TERM,ssh 进来的不算。
/// 测试(`ACA_TMUX_SOCKET` 指到假会话的服务)不动真的输入法。`ACA_IME=0` 不管,`=1` 不看这些一律管;
/// 没设 ACA_IME 时 config.toml 的 `[ime] auto = false`(设置页里关掉)也是不管
fn local() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    match std::env::var("ACA_IME").as_deref() {
        Ok("0") => return false,
        Ok("1") => return true,
        _ => {}
    }
    if std::env::var_os("ACA_TMUX_SOCKET").is_some_and(|s| !s.is_empty()) || !crate::config::ime_auto() {
        return false;
    }
    let term = if std::env::var_os("TMUX").is_some() {
        crate::data::client_term()
    } else if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        return false;
    } else {
        std::env::var("TERM").unwrap_or_default()
    };
    ["kitty", "ghostty", "wezterm"].iter().any(|t| term.contains(t))
}

/// macOS 的输入法接口(Carbon 的 TIS)
#[cfg(target_os = "macos")]
mod tis {
    use std::ffi::c_void;

    type Ref = *const c_void;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISCopyCurrentKeyboardInputSource() -> Ref;
        fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> Ref;
        fn TISGetInputSourceProperty(src: Ref, key: Ref) -> Ref;
        fn TISSelectInputSource(src: Ref) -> i32;
        static kTISPropertyInputSourceIsASCIICapable: Ref;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(r: Ref);
        fn CFBooleanGetValue(b: Ref) -> u8;
        fn CFRunLoopRunInMode(mode: Ref, seconds: f64, return_after_source_handled: u8) -> i32;
        static kCFRunLoopDefaultMode: Ref;
    }

    /// 一个输入法,自己持有一份引用
    pub struct Source(Ref);

    // CF 对象的引用计数是线程安全的
    unsafe impl Send for Source {}

    impl Drop for Source {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }

    impl Source {
        fn own(r: Ref) -> Option<Source> {
            (!r.is_null()).then_some(Source(r))
        }

        /// 先让 run loop 转一圈:「现在是哪个」在进程里有缓存,靠系统通知更新,aca 不跑 run loop 就收不到,
        /// 别处(菜单栏、快捷键、别的程序)切过之后读到的还是旧的(实测)
        pub fn current() -> Option<Source> {
            unsafe {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, 0);
                Source::own(TISCopyCurrentKeyboardInputSource())
            }
        }

        /// 最近用过的英文键盘(一般是 ABC)
        pub fn english() -> Option<Source> {
            Source::own(unsafe { TISCopyCurrentASCIICapableKeyboardLayoutInputSource() })
        }

        /// 打字母出字母(ABC、美式);拼音、注音、日文的假名模式都不是
        pub fn ascii(&self) -> bool {
            let b = unsafe { TISGetInputSourceProperty(self.0, kTISPropertyInputSourceIsASCIICapable) };
            !b.is_null() && unsafe { CFBooleanGetValue(b) } != 0
        }

        pub fn select(&self) -> bool {
            unsafe { TISSelectInputSource(self.0) == 0 }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod tis {
    pub struct Source;

    impl Source {
        pub fn current() -> Option<Source> {
            None
        }
        pub fn english() -> Option<Source> {
            None
        }
        pub fn ascii(&self) -> bool {
            true
        }
        pub fn select(&self) -> bool {
            false
        }
    }
}
