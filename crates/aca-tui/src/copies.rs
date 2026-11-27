//! `c` then a key: what to copy from the highlighted session. `c` the last reply and `b` a reference (see `data::reference`) are
//! the list's own; here are the rest: `i` aca id (for commands like `aca move <id>`), `s` session id (Claude's / Codex's own
//! conversation id, for `claude --resume`), `p` path (the session's directory), `a` all (those lines together, Path / Started in /
//! Session / Home, for pasting outside aca). One that's missing (a plain tmux session has no aca id; one that hasn't talked yet has
//! no conversation id) says why instead.

use aca_core::{t, tr};

/// The keys after `c` that copy one of these, with their names as the bottom bar says them
pub fn names() -> [(char, &'static str); 4] {
    [('i', "aca id"), ('s', t!("对话 id", "session id")), ('p', t!("路径", "path")), ('a', t!("全部", "all"))]
}

/// `c` then key: Some(Ok(text to copy)), Some(Err(why it's missing)); None = not one of these keys.
/// id: aca's session id (not registered = empty); info: `data::copy_text` (`Path: …`, `Session: …`, one per line)
pub fn pick(key: char, id: &str, info: &str) -> Option<Result<String, String>> {
    let line = |k: &str| info.lines().find_map(|l| l.strip_prefix(k)).unwrap_or("").trim().to_string();
    let (value, none) = match key {
        'i' => (id.to_string(), t!("A 接管后可用", "only after adopting with A")),
        's' => (line("Session: "), t!("暂无对话", "no conversation yet")),
        'p' => (line("Path: "), t!("目录未知", "directory unknown")),
        'a' => (info.trim().to_string(), t!("没有信息", "nothing known")),
        _ => return None,
    };
    let name = names().into_iter().find(|(k, _)| *k == key).map_or("", |(_, n)| n);
    Some(if value.is_empty() { Err(tr!("{name}:{none}", "{name}: {none}")) } else { Ok(value) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aca_core::i18n::{self, Lang};

    #[test]
    fn each_key_copies_its_one_thing_and_a_missing_one_says_why() {
        i18n::with(Lang::Zh, || {
            let info = "Path: /w/app\nSession: 5f0c-uuid";
            assert_eq!(pick('i', "3f2a9c1e", info), Some(Ok("3f2a9c1e".into())));
            assert_eq!(pick('s', "3f2a9c1e", info), Some(Ok("5f0c-uuid".into())));
            assert_eq!(pick('p', "3f2a9c1e", info), Some(Ok("/w/app".into())));
            // all: the lines together, as they are
            assert_eq!(pick('a', "3f2a9c1e", info), Some(Ok(info.into())));
            // Plain tmux session that hasn't talked yet: only the directory
            assert_eq!(pick('i', "", "Path: /w/x"), Some(Err("aca id:A 接管后可用".into())));
            assert_eq!(pick('s', "", "Path: /w/x"), Some(Err("对话 id:暂无对话".into())));
            assert_eq!(pick('x', "id", info), None);
        });
    }
}
