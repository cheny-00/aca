//! What the interface remembers on its own, all under ~/.local/state/aca/ui/: pins (pins.rs) and the last view (here).
//!
//! View = group / which sessions are listed / layout / the highlighted session, restored next time.
//! One key=value per line, **written on every change**: when the phone disconnects aca is killed outright and never gets to
//! exit normally. The filter text isn't remembered: it's temporary by nature,
//! and the commands Moshi auto-types land exactly in the filter box.

use std::path::Path;

use aca_records::paths::write_atomic;

use crate::view::{Preview, Scope};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct View {
    /// None = all groups
    pub group: Option<String>,
    pub scope: Scope,
    /// The layout on a wide screen (desktop)
    pub preview: Preview,
    /// The layout on a narrow screen (phone). Each is kept separately: the phone shows only the list, the desktop still has the
    /// preview on the right
    pub preview_narrow: Preview,
    /// The highlighted session (tmux session name)
    pub selected: Option<String>,
    /// Which workspace is being viewed (shared by phone and desktop)
    pub space: char,
}

impl Default for View {
    fn default() -> Self {
        View {
            group: None,
            scope: Scope::Open,
            preview: Preview::Auto,
            preview_narrow: Preview::Auto,
            selected: None,
            space: crate::spaces::HOME,
        }
    }
}

impl View {
    /// File missing / unreadable / a line unrecognized: use the defaults
    pub fn load(path: &Path) -> View {
        let mut v = View::default();
        // Files from before the three steps had `scope` (recent / all), `open` and `days` instead of `list`; an older version
        // still installed elsewhere may write them again, so they're read whenever `list` is missing
        let (mut list, mut old_scope, mut old_open) = (None, None, false);
        for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
            let Some((k, val)) = line.split_once('=') else { continue };
            let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
            match k {
                "group" => v.group = some(val),
                "list" => list = Scope::from_key(val),
                // Even older: "open" was a range of its own (scope=working, earlier pending)
                "scope" if matches!(val, "working" | "pending") => old_open = true,
                "scope" => old_scope = Some(val.to_string()),
                "open" => old_open |= val == "1",
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
                "space" => {
                    v.space =
                        val.chars().next().filter(|&c| crate::spaces::is_key(c) && val.chars().count() == 1).unwrap_or(crate::spaces::HOME)
                }
                _ => {}
            }
        }
        v.scope = match (list, old_scope) {
            (Some(s), _) => s,
            (None, _) if old_open => Scope::Open,
            (None, Some(_)) => Scope::Week,
            (None, None) => Scope::Open,
        };
        v
    }

    /// A group was renamed (with its subgroups): the group being viewed follows. Returns whether anything changed
    pub fn rename_group(&mut self, from: &str, to: &str) -> bool {
        let was = self.clone();
        if let Some(g) = self.group.as_mut().and_then(|g| crate::groups::renamed(g, from, to)) {
            self.group = Some(g);
        }
        *self != was
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let name = |p: Preview| match p {
            Preview::Auto => "auto",
            Preview::Below => "below",
            Preview::Focus => "focus",
            Preview::Off => "off",
        };
        let (preview, narrow) = (name(self.preview), name(self.preview_narrow));
        let text = format!(
            "group={}\nlist={}\npreview={preview}\npreview_narrow={narrow}\nselected={}\nspace={}\n",
            self.group.as_deref().unwrap_or(""),
            self.scope.key(),
            self.selected.as_deref().unwrap_or(""),
            self.space,
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
            scope: Scope::Week,
            preview: Preview::Below,
            preview_narrow: Preview::Focus,
            selected: Some("s1".into()),
            space: 'l',
        };
        v.save(&path).unwrap();
        assert_eq!(View::load(&path), v);
        // What old / new versions wrote still reads (groups could be folded once: that line is skipped)
        std::fs::write(&path, "list=all\ngarbage\nnew_key=1\nlast_space=h\nfolded=a b\twork/sub\n").unwrap();
        assert_eq!(View::load(&path), View { scope: Scope::Week, ..View::default() }); // all was a step once
        std::fs::write(&path, "space=q\n").unwrap(); // q can't be a workspace
        assert_eq!(View::load(&path).space, '1');
        std::fs::write(&path, "space=ab\n").unwrap();
        assert_eq!(View::load(&path).space, '1');
        std::fs::write(&path, "preview=0\n").unwrap(); // the old on/off switch
        assert_eq!(View::load(&path).preview, Preview::Off);
        std::fs::write(&path, "preview=1\n").unwrap();
        assert_eq!(View::load(&path).preview, Preview::Auto);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_view_from_before_the_three_steps_lands_on_the_matching_one() {
        let dir = std::env::temp_dir().join(format!("aca-view-old-{}", std::process::id()));
        let path = dir.join("view");
        let _ = std::fs::create_dir_all(&dir);
        let scope = |text: &str| {
            std::fs::write(&path, text).unwrap();
            View::load(&path).scope
        };
        // Only open ones, whatever the days and the range under it
        assert_eq!(scope("scope=all\nopen=1\nback=recent\ndays=3\n"), Scope::Open);
        assert_eq!(scope("scope=working\n"), Scope::Open); // even older: open ones were a range
        assert_eq!(scope("scope=all\nopen=0\nback=\ndays=1\n"), Scope::Week); // everything isn't a step any more
        // Recent days, any number of them
        assert_eq!(scope("scope=recent\nopen=0\nback=\ndays=1\n"), Scope::Week);
        assert_eq!(scope("open=0\nscope=recent\ndays=7\n"), Scope::Week); // order doesn't matter
        // Written by this version, then by an older one still installed (it drops `list`), then read here again
        View { scope: Scope::Week, ..View::default() }.save(&path).unwrap();
        assert_eq!(View::load(&path).scope, Scope::Week);
        assert_eq!(scope("group=\nscope=recent\nopen=1\nback=all\ndays=1\npreview=auto\n"), Scope::Open);
        // `list` wins over leftovers of the old keys
        assert_eq!(scope("scope=all\nopen=1\nlist=week\n"), Scope::Week);
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
