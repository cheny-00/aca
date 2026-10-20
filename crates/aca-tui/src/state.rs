//! 界面自己记的东西,都在 ~/.local/state/aca/ui/ 下:置顶(pins.rs)和上次的视图(这里)。
//!
//! 视图 = 分组 / 范围(和「近几天」)/ 只看开着的 / 预览开关 / 高亮的会话,下次打开恢复。一行一个 key=value,**一变就写**:
//! 手机断线时 aca 是被直接杀掉的,等不到正常退出。过滤词不记 —— 本来就是临时的,
//! 而且 Moshi 代敲的命令正是落进了过滤框。

use std::path::{Path, PathBuf};

use crate::ui::{DAYS, Preview, Scope};

/// ~/.local/state/aca/ui/<name>(`ACA_HOME` 换地方:测试动不到真的)
pub fn state_file(name: &str) -> Option<PathBuf> {
    Some(aca_core::paths::state_dir().join("ui").join(name))
}

/// 先写临时文件再改名:手机和桌面各开一个 aca 同时写,读到的也不会是半截。
/// 原来那个文件的权限照旧:收成只有自己能读的(里面有口令),不因为重写变回默认的
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_file_name(format!(".{}.tmp", path.file_name().unwrap().to_string_lossy()));
    std::fs::write(&tmp, text)?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path)
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct View {
    /// None = 全部分组
    pub group: Option<String>,
    pub scope: Scope,
    /// w:只看开着的
    pub open: bool,
    /// 按 w 之前的范围,再按 w 回到这里(在开着的里按过 * / a 是 None)
    pub back: Option<Scope>,
    /// 「近几天」的几天(`*` 轮换)
    pub days: u64,
    /// 宽屏(桌面)的布局
    pub preview: Preview,
    /// 窄屏(手机)的布局。两边各记各的:手机上只看列表,桌面照样右侧预览
    pub preview_narrow: Preview,
    /// 高亮的会话(tmux 会话名)
    pub selected: Option<String>,
    /// 折起来的分组
    pub folded: Vec<String>,
    /// 在看哪个工作区(手机和桌面共用这一份)
    pub space: char,
    /// 切到现在这个之前待的工作区:` 回那
    pub last_space: Option<char>,
}

impl Default for View {
    fn default() -> Self {
        View {
            group: None,
            scope: Scope::Recent,
            open: false,
            back: None,
            days: 1,
            preview: Preview::Auto,
            preview_narrow: Preview::Auto,
            selected: None,
            folded: vec![],
            space: crate::spaces::HOME,
            last_space: None,
        }
    }
}

fn scope(val: &str) -> Option<Scope> {
    match val {
        "all" => Some(Scope::All),
        "recent" => Some(Scope::Recent),
        _ => None,
    }
}

impl View {
    /// 文件不在 / 读不了 / 某行认不出,就用默认值
    pub fn load(path: &Path) -> View {
        let mut v = View::default();
        for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
            let Some((k, val)) = line.split_once('=') else { continue };
            let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
            match k {
                "group" => v.group = some(val),
                // 旧版「开着的」是一种范围(scope=working,更早叫 pending):就是全部里开着的
                "scope" if matches!(val, "working" | "pending") => (v.scope, v.open) = (Scope::All, true),
                "scope" => v.scope = scope(val).unwrap_or(Scope::Recent),
                "open" => v.open = val == "1",
                "back" => v.back = scope(val),
                "days" => v.days = val.parse().ok().filter(|d| DAYS.contains(d)).unwrap_or(1),
                // 旧版写的是 1 / 0,再后来有过 right(现在就是宽屏的默认)
                "preview" => {
                    v.preview = match val {
                        "below" => Preview::Below,
                        "off" | "0" => Preview::Off,
                        _ => Preview::Auto,
                    }
                }
                "preview_narrow" => {
                    v.preview_narrow = match val {
                        "focus" => Preview::Focus,
                        "off" => Preview::Off,
                        _ => Preview::Auto,
                    }
                }
                "selected" => v.selected = some(val),
                // 分组名里不会有 tab
                "folded" => v.folded = val.split('\t').filter(|g| !g.is_empty()).map(String::from).collect(),
                "space" => {
                    v.space =
                        val.chars().next().filter(|&c| crate::spaces::is_key(c) && val.chars().count() == 1).unwrap_or(crate::spaces::HOME)
                }
                "last_space" => v.last_space = val.chars().next().filter(|&c| crate::spaces::is_key(c) && val.chars().count() == 1),
                _ => {}
            }
        }
        v
    }

    /// 分组改了名(连子分组):在看的组、折起来的跟过去。返回有没有变
    pub fn rename_group(&mut self, from: &str, to: &str) -> bool {
        let was = self.clone();
        if let Some(g) = self.group.as_mut().and_then(|g| crate::groups::renamed(g, from, to)) {
            self.group = Some(g);
        }
        for g in &mut self.folded {
            if let Some(n) = crate::groups::renamed(g, from, to) {
                *g = n;
            }
        }
        *self != was
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let scope = |s: Scope| match s {
            Scope::Recent => "recent",
            Scope::All => "all",
        };
        let (back, scope) = (self.back.map_or("", scope), scope(self.scope));
        let name = |p: Preview| match p {
            Preview::Auto => "auto",
            Preview::Below => "below",
            Preview::Focus => "focus",
            Preview::Off => "off",
        };
        let (preview, narrow) = (name(self.preview), name(self.preview_narrow));
        let text = format!(
            "group={}\nscope={scope}\nopen={}\nback={back}\ndays={}\npreview={preview}\npreview_narrow={narrow}\nselected={}\nfolded={}\nspace={}\nlast_space={}\n",
            self.group.as_deref().unwrap_or(""),
            u8::from(self.open),
            self.days,
            self.selected.as_deref().unwrap_or(""),
            self.folded.join("\t"),
            self.space,
            self.last_space.map(String::from).unwrap_or_default(),
        );
        write_atomic(path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_round_trips_and_tolerates_junk() {
        let dir = std::env::temp_dir().join(format!("aca-view-test-{}", std::process::id()));
        let path = dir.join("view");
        assert_eq!(View::load(&path), View::default()); // 还没有文件
        let v = View {
            group: Some("work/sub".into()),
            scope: Scope::Recent,
            open: true,
            back: Some(Scope::All),
            days: 7,
            preview: Preview::Below,
            preview_narrow: Preview::Focus,
            selected: Some("s1".into()),
            folded: vec!["a b".into(), "work/sub".into()],
            space: 'l',
            last_space: Some('h'),
        };
        v.save(&path).unwrap();
        assert_eq!(View::load(&path), v);
        std::fs::write(&path, "scope=all\ngarbage\nnew_key=1\n").unwrap(); // 旧版 / 新版写的也能读
        assert_eq!(View::load(&path), View { scope: Scope::All, ..View::default() });
        std::fs::write(&path, "scope=working\nback=working\n").unwrap(); // 旧版的「开着的」是一种范围
        assert_eq!(View::load(&path), View { scope: Scope::All, open: true, back: None, ..View::default() });
        std::fs::write(&path, "space=q\n").unwrap(); // q 不能当工作区
        assert_eq!(View::load(&path).space, '1');
        std::fs::write(&path, "space=ab\n").unwrap();
        assert_eq!(View::load(&path).space, '1');
        std::fs::write(&path, "last_space=q\n").unwrap();
        assert_eq!(View::load(&path).last_space, None);
        std::fs::write(&path, "days=5\n").unwrap(); // 不是 * 轮换得到的天数
        assert_eq!(View::load(&path).days, 1);
        std::fs::write(&path, "preview=0\n").unwrap(); // 旧版的开关
        assert_eq!(View::load(&path).preview, Preview::Off);
        std::fs::write(&path, "preview=1\n").unwrap();
        assert_eq!(View::load(&path).preview, Preview::Auto);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewriting_a_file_keeps_the_permissions_it_had() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aca-atomic-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        write_atomic(&path, "a = 1\n").unwrap();
        let fresh = mode(&path);
        assert_ne!(fresh, 0o600, "新建的文件照 umask 来");
        // 收紧过的(里面有口令):重写之后还是只有自己能读
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&path, "a = 2\n").unwrap();
        assert_eq!((mode(&path), std::fs::read_to_string(&path).unwrap().as_str()), (0o600, "a = 2\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
