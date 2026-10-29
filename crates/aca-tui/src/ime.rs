//! Input method: letter keys in the list are commands, but with an input method like Pinyin on, letters go into its candidate
//! box first and the terminal never sends them to aca.
//! On the desktop, switch to English (ABC) while in aca, and back to the original where you type (search, input boxes, the
//! temporary terminal) and when handing over the terminal (entering a session, editor, fzf, quitting).
//! If it was English already, nothing changes, so nothing needs switching back.
//!
//! - Only the local machine's terminal: the phone goes through Moshi and its iPhone keyboard is out of reach; an aca opened from
//!   the phone doesn't touch the Mac's input method either
//! - With "Automatically switch to a document's input source" off in System Settings there's one input method for the whole
//!   system, so always switch back when leaving
//! - tmux popups get no focus events (panes do): switching to another app with the popup open leaves English there too; it
//!   switches back only when entering a session / closing

use std::sync::Mutex;

enum St {
    /// Not managed (phone, tests, not macOS)
    Off,
    /// Somewhere handed over (typing, inside a session), not holding it
    Out,
    /// In the list: if it switched to English, remembers the original; None if it was English already
    In(Option<tis::Source>),
}

static ST: Mutex<St> = Mutex::new(St::Off);

/// When aca opens: manage only in a local terminal, and then switch right away (so keys pressed as soon as the popup appears
/// aren't eaten by Pinyin)
pub fn start() {
    if !local() {
        return;
    }
    if let Ok(mut st) = ST.lock() {
        *st = St::Out;
    }
    english(true);
}

/// true: switch to English, remembering the original; false: switch back to the original. Same as last time = do nothing (it's
/// called every round)
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

/// The setting changed (settings page, config.toml's [ime] auto): decide again by the current setting whether to manage.
/// Managing: first mark it as handed over, the main loop switches on its own next frame; no longer managing: switch back to
/// the original, then let go
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

/// ACA_IME is set: the settings page marks it (this terminal follows it)
pub fn forced() -> bool {
    std::env::var("ACA_IME").is_ok_and(|v| v == "0" || v == "1")
}

/// Managing the input method (desktop)
pub fn active() -> bool {
    ST.lock().is_ok_and(|st| !matches!(*st, St::Off))
}

/// Switch back even on a crash (a normal exit and errors propagated by `?` take the same path; calling it twice is harmless)
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        english(false);
    }
}

/// Whether the keys come from a local terminal: inside tmux, look at this client's terminal type (popup = the `--client` one);
/// a phone connected over mosh is xterm-256color or the like; outside tmux, look at TERM; ssh sessions don't count.
/// Tests (`ACA_TMUX_SOCKET` pointing at a server of fake sessions) don't touch the real input method. `ACA_IME=0` never
/// manages, `=1` always manages regardless of all this;
/// without ACA_IME, config.toml's `[ime] auto = false` (turned off on the settings page) also means not managing
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

/// macOS's input method API (Carbon's TIS)
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

    /// An input method, holding its own reference
    pub struct Source(Ref);

    // CF objects' reference counting is thread-safe
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

        /// Spin the run loop once first: "which one is current" is cached in the process and updated by system notifications,
        /// which aca doesn't receive without running a run loop, so after a switch elsewhere (menu bar, shortcut, another
        /// program) it still reads the old one (tested)
        pub fn current() -> Option<Source> {
            unsafe {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, 0);
                Source::own(TISCopyCurrentKeyboardInputSource())
            }
        }

        /// The most recently used English keyboard (usually ABC)
        pub fn english() -> Option<Source> {
            Source::own(unsafe { TISCopyCurrentASCIICapableKeyboardLayoutInputSource() })
        }

        /// Typing letters gives letters (ABC, U.S.); Pinyin, Zhuyin and Japanese kana modes don't
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
