//! What the interface remembers on its own, all under ~/.local/state/aca/ui/: pins (pins.rs) and the last view (here).
//!
//! View = group / range (and "recent days") / open ones only / preview on-off / the highlighted session, restored next time.
//! One key=value per line, **written on every change**: when the phone disconnects aca is killed outright and never gets to
//! exit normally. The filter text isn't remembered: it's temporary by nature,
//! and the commands Moshi auto-types land exactly in the filter box.

use std::path::{Path, PathBuf};

use crate::ui::{DAYS, Preview, Scope};

/// ~/.local/state/aca/ui/<name> (`ACA_HOME` moves it: tests can't touch the real one)
pub fn state_file(name: &str) -> Option<PathBuf> {
    Some(aca_core::paths::state_dir().join("ui").join(name))
}

/// Write a temporary file, then rename: when the aca on the phone and the one on the desktop write at once, a reader never sees
/// half a file. The original file's permissions are kept: one tightened to owner-only (it holds secrets) doesn't go back to the
/// default on a rewrite
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
    /// None = all groups
    pub group: Option<String>,
    pub scope: Scope,
    /// w: open ones only
    pub open: bool,
    /// The range before w was pressed; w again returns here (None if * / a was pressed among the open ones)
    pub back: Option<Scope>,
    /// How many days "recent days" means (`*` cycles)
    pub days: u64,
    /// The layout on a wide screen (desktop)
    pub preview: Preview,
    /// The layout on a narrow screen (phone). Each is kept separately: the phone shows only the list, the desktop still has the
    /// preview on the right
    pub preview_narrow: Preview,
    /// The highlighted session (tmux session name)
    pub selected: Option<String>,
    /// Folded groups
    pub folded: Vec<String>,
    /// Which workspace is being viewed (shared by phone and desktop)
    pub space: char,
    /// The workspace you were in before switching to this one: ` goes back there
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
    /// File missing / unreadable / a line unrecognized: use the defaults
    pub fn load(path: &Path) -> View {
        let mut v = View::default();
        for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
            let Some((k, val)) = line.split_once('=') else { continue };
            let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
            match k {
                "group" => v.group = some(val),
                // In old versions "open" was a range (scope=working, earlier pending): that's the open ones among all
                "scope" if matches!(val, "working" | "pending") => (v.scope, v.open) = (Scope::All, true),
                "scope" => v.scope = scope(val).unwrap_or(Scope::Recent),
                "open" => v.open = val == "1",
                "back" => v.back = scope(val),
                "days" => v.days = val.parse().ok().filter(|d| DAYS.contains(d)).unwrap_or(1),
                // Old versions wrote 1 / 0; later there was right (now simply the wide-screen default)
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
                // Group names never contain a tab
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

    /// A group was renamed (with its subgroups): the group being viewed and the folded ones follow. Returns whether anything changed
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
        assert_eq!(View::load(&path), View::default()); // no file yet
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
        std::fs::write(&path, "scope=all\ngarbage\nnew_key=1\n").unwrap(); // what old / new versions wrote still reads
        assert_eq!(View::load(&path), View { scope: Scope::All, ..View::default() });
        std::fs::write(&path, "scope=working\nback=working\n").unwrap(); // in old versions "open" was a range
        assert_eq!(View::load(&path), View { scope: Scope::All, open: true, back: None, ..View::default() });
        std::fs::write(&path, "space=q\n").unwrap(); // q can't be a workspace
        assert_eq!(View::load(&path).space, '1');
        std::fs::write(&path, "space=ab\n").unwrap();
        assert_eq!(View::load(&path).space, '1');
        std::fs::write(&path, "last_space=q\n").unwrap();
        assert_eq!(View::load(&path).last_space, None);
        std::fs::write(&path, "days=5\n").unwrap(); // not a day count * cycles through
        assert_eq!(View::load(&path).days, 1);
        std::fs::write(&path, "preview=0\n").unwrap(); // the old on/off switch
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
        // Tightened (it holds secrets): still owner-only after a rewrite
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&path, "a = 2\n").unwrap();
        assert_eq!((mode(&path), std::fs::read_to_string(&path).unwrap().as_str()), (0o600, "a = 2\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
