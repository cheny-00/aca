//! 便签:写给某个会话的下一句话,平时就是便签;按 `E`(桌面 kitty 里 Shift+Enter 也行)进去时才填进它的输入框
//! (不发送),填完就删 —— 像 Claude Code 的 stash。
//! 存在 ~/.local/state/aca/ui/notes(JSON)。
//! 按会话的键记(登记的是它的 id,改名、重启都不变;普通 tmux 会话是会话名)。
//! 分组也有一条(停在分组上 `e`):记这一组整体的想法,不填进哪个会话;组改名跟着改,删组时跟着进回收站。
//! 和置顶一样,每次刷新都重读,改之前先重读,手机和桌面各开一个 aca 不会互相覆盖。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use aca_core::tr;

use crate::data::Session;
use crate::state::{state_file, write_atomic};

#[derive(Default)]
pub struct Notes {
    /// None = 只在内存里(单元测试、--snapshot)
    path: Option<PathBuf>,
    map: BTreeMap<String, String>,
}

/// 会话记在哪个键下
pub fn key(s: &Session) -> String {
    if s.deck_id.is_empty() { format!("tmux:{}", s.name) } else { s.deck_id.clone() }
}

const GROUP: &str = "group:";

/// 分组记在哪个键下(界面里 terms / glances 按分组记的也用它)。tmux 会话名里没有「:」,和会话的撞不上
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

    /// 读完就和文件断开,之后的改动只在内存里
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

    /// 存便签;只剩空白 = 删掉
    pub fn set(&mut self, key: &str, text: &str) -> std::io::Result<()> {
        self.reload();
        if text.trim().is_empty() {
            self.map.remove(key);
        } else {
            self.map.insert(key.to_string(), text.to_string());
        }
        self.save()
    }

    /// 分组 from 改名成 to:它和子分组的便签跟过去(登记表那边改成了才调)
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

    /// 填进会话之后删掉
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

/// ^G 用的编辑器:$VISUAL → $EDITOR → vi(可以带参数,比如 `code -w`)
pub(crate) fn editor() -> String {
    ["VISUAL", "EDITOR"].iter().filter_map(|k| std::env::var(k).ok()).find(|v| !v.trim().is_empty()).unwrap_or_else(|| "vi".into())
}

/// 底栏提示里写的编辑器名字
pub fn editor_name() -> String {
    let e = editor();
    let first = e.split_whitespace().next().unwrap_or("vi");
    first.rsplit('/').next().unwrap_or(first).to_string()
}

/// ^G:交给编辑器写,返回写完的内容(去掉结尾的换行)。编辑器没正常退出(比如 nvim 的 `:cq`)= 不算数
pub fn edit_in_editor(text: &str) -> Result<String, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = std::env::temp_dir().join(format!("aca-note-{}.md", std::process::id()));
    let write = |p: &PathBuf| -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p)?;
        // 空的就给空文件,不然编辑器里开头多一个空行
        f.write_all(if text.is_empty() { String::new() } else { format!("{text}\n") }.as_bytes())
    };
    write(&path).map_err(|e| tr!("无法写入临时文件:{e}", "can't write the temp file: {e}"))?;
    let ed = editor();
    // 交给 sh:$EDITOR 可以带参数
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
        desk.set("b", "看看 CI").unwrap(); // 写之前重读,a 不丢
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
