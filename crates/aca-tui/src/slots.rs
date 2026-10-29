//! Each account's color slot on the billing page: an account seen for the first time takes the lowest free slot and keeps it.
//! The color follows the account, not "who spent most in this period": switching periods, showing only some accounts, adding
//! accounts later, existing accounts never change color.
//! There are only as many slots as palette colors (theme's series, 5 by default, more can be listed in config.toml): accounts
//! arriving once they're full are drawn gray and merged into 「其他」 (other) in the trend;
//! when the palette shrinks, slots beyond it are void and those accounts wait for a free slot like newcomers;
//! accounts gone from the records (account directory deleted) give up their slots. Kept in ~/.local/state/aca/ui/colors, one
//! per line: `<slot>\t<account>`

use std::path::Path;

/// names: the accounts there are now, first come first served (passed in by total spend, most first); size: how many colors
/// the palette has. With path None, nothing is read or written, slots just go in order
pub fn assign(path: Option<&Path>, names: &[String], size: usize) -> Vec<(String, usize)> {
    let text = path.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let mut got: Vec<(String, usize)> = vec![];
    for l in text.lines() {
        if let Some((slot, name)) = l.split_once('\t')
            && let Ok(slot) = slot.parse::<usize>()
            && slot < size
            && names.iter().any(|n| n == name)
            && !got.iter().any(|(n, s)| n == name || *s == slot)
        {
            got.push((name.to_string(), slot));
        }
    }
    for n in names {
        if got.iter().any(|(x, _)| x == n) {
            continue;
        }
        let Some(free) = (0..size).find(|s| !got.iter().any(|(_, x)| x == s)) else { break };
        got.push((n.clone(), free));
    }
    got.sort_by_key(|x| x.1);
    let out: String = got.iter().map(|(n, s)| format!("{s}\t{n}\n")).collect();
    if let Some(p) = path
        && out != text
    {
        let _ = crate::state::write_atomic(p, &out);
    }
    got
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_keeps_its_slot_and_new_ones_take_the_free_ones() {
        let p = std::env::temp_dir().join(format!("aca-slots-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(assign(Some(&p), &v(&["Claude work", "Claude home"]), 5), vec![("Claude work".into(), 0), ("Claude home".into(), 1)]);
        // home now spends more and a new account came: the old ones keep their slots, the new one takes a free slot
        let got = assign(Some(&p), &v(&["Claude home", "Codex x", "Claude work"]), 5);
        assert_eq!(got, vec![("Claude work".into(), 0), ("Claude home".into(), 1), ("Codex x".into(), 2)]);
        // work's directory was deleted: its slot goes to the next new account
        let got = assign(Some(&p), &v(&["Claude home", "Codex x", "Codex y"]), 5);
        assert_eq!(got, vec![("Codex y".into(), 0), ("Claude home".into(), 1), ("Codex x".into(), 2)]);
        // Once full, no slot
        let many = v(&["Claude home", "Codex x", "Codex y", "a", "b", "c"]);
        let got = assign(Some(&p), &many, 5);
        assert_eq!(got.len(), 5);
        assert!(!got.iter().any(|(n, _)| n == "c"));
        // Palette shrunk: slots beyond it are void; grown again, the waiting ones take free slots in order
        let got = assign(Some(&p), &many, 3);
        assert_eq!(got.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["Codex y", "Claude home", "Codex x"]);
        let got = assign(Some(&p), &many, 6);
        std::fs::remove_file(&p).unwrap();
        assert_eq!(got[3..], [("a".into(), 3), ("b".into(), 4), ("c".into(), 5)]);
        assert_eq!(assign(None, &v(&["a"]), 5), vec![("a".into(), 0)]);
    }
}
