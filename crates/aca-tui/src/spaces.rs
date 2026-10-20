//! 工作区:会话分到几个用一个键(小写字母 / 数字)命名的工作区里,一次只看一个 —— 工作和生活分开。
//! `q`+键 切过去,`Q`+键 把会话(折叠行上 = 整组)送过去,`[` `]` 前后换,`` ` `` 回刚才那个,`S` 列出来(`^R` 改名)。
//! `h` 是隐藏的:`S` 里不列、`[` `]` 跳过,要 `q h` 进(出来以后 `` ` `` 也回得去)。
//! 会话在哪:自己分过的 → 子会话跟父会话 → 跟它的组(折叠行上 `Q` 记下的)→ `1`(见 `ui::space_of`)。
//! 组也记,是为了以后这个组里新开的(终端里 `aca new` 开的也算)直接落在那边,藏进 h 的组不会漏出来。
//! 按 `notes::key` 记(登记的会话按 id,改名、重启都跟着)。
//! 存在 ~/.local/state/aca/ui/spaces,一行一条:`in\t<会话的键>\t<工作区>`、`group\t<profile>\t<分组>\t<工作区>`、
//! `name\t<工作区>\t<名字>`(改过名的)。认不出的行原样留着(以后的版本写的,存的时候不丢)。
//! 和置顶一样每次刷新都重读、改之前先重读,手机和桌面各开一个 aca 不会互相覆盖。

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::state::{state_file, write_atomic};

/// 没分过的会话在这
pub const HOME: char = '1';
/// 隐藏的工作区
pub const HIDDEN: char = 'h';

/// 能当工作区的键:小写字母和数字。q 不算(`q q` 是退出)
pub fn is_key(c: char) -> bool {
    (c.is_ascii_lowercase() || c.is_ascii_digit()) && c != 'q'
}

/// 组:(profile, 分组)。登记的会话 profile 都是 `data::REGISTERED`;普通 tmux 会话是 ("", "-")
pub type Group = (String, String);

#[derive(Clone, Default)]
pub struct Spaces {
    /// None = 只在内存里(单元测试、--snapshot)
    path: Option<PathBuf>,
    of: BTreeMap<String, char>,
    groups: BTreeMap<Group, char>,
    names: BTreeMap<char, String>,
    /// 认不出的行
    other: Vec<String>,
}

impl Spaces {
    pub fn default_path() -> Option<PathBuf> {
        state_file("spaces")
    }

    pub fn open(path: Option<PathBuf>) -> Self {
        let mut s = Spaces { path, ..Spaces::default() };
        s.reload();
        s
    }

    /// 读完就和文件断开,之后的改动只在内存里(--snapshot 喂按键时不能写真文件)
    pub fn detached(mut self) -> Self {
        self.path = None;
        self
    }

    pub fn reload(&mut self) {
        let Some(p) = &self.path else { return };
        let (mut of, mut groups, mut names, mut other) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), vec![]);
        let one = |s: &str| {
            let mut cs = s.chars();
            cs.next().filter(|&c| is_key(c) && cs.next().is_none())
        };
        for l in std::fs::read_to_string(p).unwrap_or_default().lines() {
            let f: Vec<&str> = l.split('\t').collect();
            match f.as_slice() {
                ["in", k, c] if !k.is_empty() => {
                    if let Some(c) = one(c) {
                        of.insert(k.to_string(), c);
                    }
                }
                ["group", p, g, c] if !g.is_empty() => {
                    if let Some(c) = one(c) {
                        groups.insert((p.to_string(), g.to_string()), c);
                    }
                }
                ["name", c, n] if !n.is_empty() => {
                    if let Some(c) = one(c) {
                        names.insert(c, n.to_string());
                    }
                }
                // 认得的行写坏了就扔掉;不认得的留着
                ["in" | "group" | "name", ..] | [""] => {}
                _ => other.push(l.to_string()),
            }
        }
        (self.of, self.groups, self.names, self.other) = (of, groups, names, other);
    }

    /// 会话自己分到了哪(没分过 = None,由调用方决定跟父会话、跟组还是在 1)
    pub fn of(&self, key: &str) -> Option<char> {
        self.of.get(key).copied()
    }

    /// 组记在哪(没记过 = None)
    pub fn group_of(&self, profile: &str, group: &str) -> Option<char> {
        self.groups.get(&(profile.to_string(), group.to_string())).copied()
    }

    /// 只改内存(算「改完之后各在哪」用):Some = 记到那,None = 不记
    pub fn set(&mut self, key: &str, c: Option<char>) {
        match c {
            Some(c) => self.of.insert(key.to_string(), c),
            None => self.of.remove(key),
        };
    }

    pub fn set_group(&mut self, g: &Group, c: Option<char>) {
        match c {
            Some(c) => self.groups.insert(g.clone(), c),
            None => self.groups.remove(g),
        };
    }

    /// 显示的名字:改过的,没改过就是键本身
    pub fn name(&self, c: char) -> String {
        self.names.get(&c).cloned().unwrap_or_else(|| c.to_string())
    }

    pub fn renamed(&self, c: char) -> bool {
        self.names.contains_key(&c)
    }

    /// 改会话和组的记录(Some = 记到那,None = 不记),先重读再存。
    /// keep:还在的会话,别的会话的记录顺手清掉(删掉的会话不留在文件里);组的不清,组空了以后新开的还要落回去
    pub fn assign(
        &mut self,
        sessions: &[(String, Option<char>)],
        groups: &[(Group, Option<char>)],
        keep: &dyn Fn(&str) -> bool,
    ) -> std::io::Result<()> {
        self.reload();
        self.of.retain(|k, _| keep(k));
        for (k, c) in sessions {
            self.set(k, *c);
        }
        for (g, c) in groups {
            self.set_group(g, *c);
        }
        self.save()
    }

    /// 这些组删掉了:记录不留(以后再建个同名的不该还落在老地方),先重读再存
    pub fn forget_groups(&mut self, gone: &dyn Fn(&str, &str) -> bool) -> std::io::Result<()> {
        self.reload();
        let n = self.groups.len();
        self.groups.retain(|(p, g), _| !gone(p, g));
        if self.groups.len() == n {
            return Ok(());
        }
        self.save()
    }

    /// 分组改了名(连子分组):记的组跟过去,先重读再存
    pub fn rename_group(&mut self, profile: &str, from: &str, to: &str) -> std::io::Result<()> {
        self.reload();
        let moved: Vec<(Group, char)> = self
            .groups
            .iter()
            .filter(|((p, g), _)| p == profile && crate::groups::renamed(g, from, to).is_some())
            .map(|(g, c)| (g.clone(), *c))
            .collect();
        if moved.is_empty() {
            return Ok(());
        }
        for ((p, g), c) in moved {
            self.groups.remove(&(p.clone(), g.clone()));
            self.groups.insert((p, crate::groups::renamed(&g, from, to).unwrap()), c);
        }
        self.save()
    }

    /// 改名;空 = 改回用键当名字
    pub fn rename(&mut self, c: char, name: &str) -> std::io::Result<()> {
        self.reload();
        let name = name.trim();
        if name.is_empty() || name == c.to_string() {
            self.names.remove(&c);
        } else {
            // 名字里不能有换行 / tab(一行一条)
            self.names.insert(c, name.replace(['\t', '\n'], " "));
        }
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(p) = &self.path else { return Ok(()) };
        let mut text = String::new();
        for (c, n) in &self.names {
            text.push_str(&format!("name\t{c}\t{n}\n"));
        }
        for ((p, g), c) in &self.groups {
            text.push_str(&format!("group\t{p}\t{g}\t{c}\n"));
        }
        for (k, c) in &self.of {
            text.push_str(&format!("in\t{k}\t{c}\n"));
        }
        for l in &self.other {
            text.push_str(l);
            text.push('\n');
        }
        write_atomic(p, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_instances_share_the_file_and_unknown_lines_survive() {
        let dir = std::env::temp_dir().join(format!("aca-spaces-test-{}", std::process::id()));
        let path = dir.join("spaces");
        let all = |_: &str| true;
        let mut phone = Spaces::open(Some(path.clone()));
        let mut desk = Spaces::open(Some(path.clone()));
        let fun: Group = ("default".into(), "fun".into());
        phone.assign(&[("a".into(), Some('l')), ("b".into(), Some('l'))], &[(fun.clone(), Some('h'))], &all).unwrap();
        desk.rename('l', "生活").unwrap(); // desk 内存里还没有 a b,写之前重读所以不丢
        let s = Spaces::open(Some(path.clone()));
        assert_eq!((s.of("a"), s.of("b"), s.of("c")), (Some('l'), Some('l'), None));
        assert_eq!((s.group_of("default", "fun"), s.group_of("other", "fun")), (Some('h'), None));
        assert_eq!((s.name('l'), s.name('2')), ("生活".to_string(), "2".to_string()));
        phone.assign(&[("a".into(), None)], &[], &|k| k != "b").unwrap(); // a 不记了;b 删掉了;组的留着
        let s = Spaces::open(Some(path.clone()));
        assert_eq!((s.of("a"), s.of("b"), s.group_of("default", "fun")), (None, None, Some('h')));
        phone.rename('l', " ").unwrap(); // 空名字 = 改回用键
        assert!(!Spaces::open(Some(path.clone())).renamed('l'));
        // 组删了:它的记录不留,别的组的还在
        phone
            .assign(&[], &[(("default".into(), "fun/app".into()), Some('h')), (("default".into(), "life".into()), Some('l'))], &all)
            .unwrap();
        assert_eq!((phone.group_of("default", "fun"), phone.group_of("default", "fun/app")), (Some('h'), Some('h')));
        desk.forget_groups(&|p, g| p == "default" && g.starts_with("fun")).unwrap();
        let s = Spaces::open(Some(path.clone()));
        assert_eq!(
            (s.group_of("default", "fun"), s.group_of("default", "fun/app"), s.group_of("default", "life")),
            (None, None, Some('l'))
        );
        // 写坏的行扔掉;不认得的(以后的版本写的)存的时候原样留着
        std::fs::write(&path, "in\tx\tq\nin\ty\tAB\nname\t9\t\ngroup\tp\t\t2\nin\tz\t3\nlater\tv\t1\n").unwrap();
        let mut s = Spaces::open(Some(path.clone()));
        assert_eq!((s.of("x"), s.of("y"), s.of("z"), s.renamed('9')), (None, None, Some('3'), false));
        s.assign(&[("w".into(), Some('2'))], &[], &all).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("later\tv\t1\n") && text.contains("in\tw\t2\n") && !text.contains("AB"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_are_lowercase_letters_and_digits_but_not_q() {
        assert!(is_key('a') && is_key('h') && is_key('0') && is_key('9'));
        assert!(!is_key('q') && !is_key('A') && !is_key('[') && !is_key('中'));
    }
}
