//! Notes: the next thing to say to a session, otherwise just a note; only when you go in with `E` (Shift+Enter works too in
//! kitty on the desktop) is it filled into the session's input box
//! (not sent), then deleted: like Claude Code's stash.
//! Stored in ~/.local/state/aca/ui/notes (JSON).
//! Keyed by the session's key (for registered ones their id, unchanged by renames and restarts; for plain tmux sessions the
//! session name).
//! A group can have one too (`e` on a group): thoughts on the group as a whole, not filled into any session; it follows the
//! group's renames and goes to the trash along with the group when it's deleted.
//! Like pins, reread on every refresh and before every change, so an aca open on the phone and one on the desktop don't
//! overwrite each other.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use aca_core::tr;

use crate::data::Session;
use crate::state::{state_file, write_atomic};

#[derive(Default)]
pub struct Notes {
    /// None = in memory only (unit tests, --snapshot)
    path: Option<PathBuf>,
    map: BTreeMap<String, String>,
}

/// The key a session is stored under
pub fn key(s: &Session) -> String {
    if s.deck_id.is_empty() { format!("tmux:{}", s.name) } else { s.deck_id.clone() }
}

const GROUP: &str = "group:";

/// The key a group is stored under (the interface's per-group terms / glances use it too). tmux session names never contain
/// `:`, so it can't clash with a session's
pub fn group_key(g: &str) -> String {
    format!("{GROUP}{g}")
}

pub fn is_group_key(k: &str) -> bool {
    k.starts_with(GROUP)
}

impl Notes {
    pub fn default_path() -> Option<PathBuf> {
        state_file("notes")
    }

    pub fn open(path: Option<PathBuf>) -> Self {
        let mut n = Notes { path, map: BTreeMap::new() };
        n.reload();
        n
    }

    /// Cut loose from the file after reading; later changes stay in memory only
    pub fn detached(mut self) -> Self {
        self.path = None;
        self
    }

    pub fn reload(&mut self) {
        let Some(p) = &self.path else { return };
        self.map = std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    /// Save a note; nothing but whitespace = delete it
    pub fn set(&mut self, key: &str, text: &str) -> std::io::Result<()> {
        self.reload();
        if text.trim().is_empty() {
            self.map.remove(key);
        } else {
            self.map.insert(key.to_string(), text.to_string());
        }
        self.save()
    }

    /// Group from renamed to to: its notes and its subgroups' follow (called only once the registry rename succeeded)
    pub fn rename_group(&mut self, from: &str, to: &str) -> std::io::Result<()> {
        let (from, to) = (from.trim_matches('/'), to.trim_matches('/'));
        self.reload();
        let olds: Vec<String> =
            self.map.keys().filter(|k| k.strip_prefix(GROUP).is_some_and(|g| crate::groups::within(g, from))).cloned().collect();
        if olds.is_empty() || from == to {
            return Ok(());
        }
        let moved: Vec<(String, String)> = olds
            .into_iter()
            .filter_map(|k| {
                let text = self.map.remove(&k)?;
                Some((group_key(&crate::groups::renamed(&k[GROUP.len()..], from, to)?), text))
            })
            .collect();
        self.map.extend(moved);
        self.save()
    }

    /// Delete it once it's filled into the session
    pub fn take(&mut self, key: &str) -> std::io::Result<Option<String>> {
        self.reload();
        let t = self.map.remove(key);
        self.save()?;
        Ok(t)
    }

    fn save(&self) -> std::io::Result<()> {
        match &self.path {
            Some(p) => write_atomic(p, &(serde_json::to_string_pretty(&self.map).unwrap_or_default() + "\n")),
            None => Ok(()),
        }
    }
}

/// The editor for ^G: $VISUAL → $EDITOR → vi (may carry arguments, e.g. `code -w`)
pub(crate) fn editor() -> String {
    ["VISUAL", "EDITOR"].iter().filter_map(|k| std::env::var(k).ok()).find(|v| !v.trim().is_empty()).unwrap_or_else(|| "vi".into())
}

/// The editor name written in the bottom bar hint
pub fn editor_name() -> String {
    let e = editor();
    let first = e.split_whitespace().next().unwrap_or("vi");
    first.rsplit('/').next().unwrap_or(first).to_string()
}

/// ^G: let the editor write it; returns the result (trailing newlines stripped). The editor not exiting cleanly (e.g. nvim's
/// `:cq`) = doesn't count
pub fn edit_in_editor(text: &str) -> Result<String, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = std::env::temp_dir().join(format!("aca-note-{}.md", std::process::id()));
    let write = |p: &PathBuf| -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p)?;
        // Empty gives an empty file, otherwise the editor starts with an extra blank line
        f.write_all(if text.is_empty() { String::new() } else { format!("{text}\n") }.as_bytes())
    };
    write(&path).map_err(|e| tr!("无法写入临时文件:{e}", "can't write the temp file: {e}"))?;
    let ed = editor();
    // Through sh: $EDITOR may carry arguments
    let st = Command::new("sh").arg("-c").arg(format!("{ed} \"$1\"")).arg("sh").arg(&path).status();
    let got = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    match st {
        Ok(s) if s.success() => got.map(|t| t.trim_matches('\n').to_string()).map_err(|e| e.to_string()),
        Ok(_) => Err(tr!("{} 未正常退出,便签未修改", "{} didn't exit cleanly; note unchanged", editor_name())),
        Err(e) => Err(tr!("无法启动 {}:{e}", "can't start {}: {e}", editor_name())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_instances_share_notes_and_blank_means_delete() {
        let dir = std::env::temp_dir().join(format!("aca-notes-test-{}", std::process::id()));
        let path = dir.join("notes");
        let mut phone = Notes::open(Some(path.clone()));
        let mut desk = Notes::open(Some(path.clone()));
        phone.set("a", "下一步:补测试\n再跑一遍").unwrap();
        desk.set("b", "看看 CI").unwrap(); // rereads before writing, a isn't lost
        let fresh = Notes::open(Some(path.clone()));
        assert_eq!(fresh.get("a"), Some("下一步:补测试\n再跑一遍"));
        assert_eq!(fresh.get("b"), Some("看看 CI"));
        assert_eq!(phone.take("b").unwrap().as_deref(), Some("看看 CI"));
        desk.set("a", "  \n").unwrap();
        assert!(Notes::open(Some(path.clone())).map.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_group_note_follows_the_group_and_its_subgroups_when_renamed() {
        let mut n = Notes::default();
        for (k, t) in [("fun", "整体"), ("fun/a", "子组"), ("funny", "别的组"), ("id-1", "会话的")] {
            n.set(&if k.starts_with("id-") { k.to_string() } else { group_key(k) }, t).unwrap();
        }
        n.rename_group("fun", "play/fun").unwrap();
        let groups: Vec<(&str, &str)> =
            n.map.iter().filter(|(k, _)| is_group_key(k)).map(|(k, t)| (&k[GROUP.len()..], t.as_str())).collect();
        assert_eq!(groups, [("funny", "别的组"), ("play/fun", "整体"), ("play/fun/a", "子组")]);
        assert_eq!(n.get("id-1"), Some("会话的"));
    }
}
