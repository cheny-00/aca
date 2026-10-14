//! 计费面板里每个账号的颜色位子:第一次见到的账号拿最小的空位,之后不变。颜色跟着账号走,
//! 不跟「这个时段谁花得多」走 —— 切时段、只看几个账号、以后再加账号,老账号都不换色。
//! 位子只有色盘那么多(theme 的 series,默认 5 个,config.toml 里可以多写几个):满了之后来的账号画灰色,在走势里并进「其他」;
//! 色盘改小了,超出去的位子作废,那几个账号跟新来的一样等空位;
//! 记录里没了的账号(账号目录删了)把位子让出来。记在 ~/.local/state/aca/ui/colors,一行一个:`<位子>\t<账号>`

use std::path::Path;

/// names:现在有的账号,先来的先拿位子(传进来时按总花费从多到少);size:色盘有几个颜色。path 为 None 时不读不写,只按顺序分
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
        // home 花得更多了、又来了个新账号:老的不换位子,新的拿空位
        let got = assign(Some(&p), &v(&["Claude home", "Codex x", "Claude work"]), 5);
        assert_eq!(got, vec![("Claude work".into(), 0), ("Claude home".into(), 1), ("Codex x".into(), 2)]);
        // work 的目录删了:位子让出来给下一个新账号
        let got = assign(Some(&p), &v(&["Claude home", "Codex x", "Codex y"]), 5);
        assert_eq!(got, vec![("Codex y".into(), 0), ("Claude home".into(), 1), ("Codex x".into(), 2)]);
        // 满了的不给位子
        let many = v(&["Claude home", "Codex x", "Codex y", "a", "b", "c"]);
        let got = assign(Some(&p), &many, 5);
        assert_eq!(got.len(), 5);
        assert!(!got.iter().any(|(n, _)| n == "c"));
        // 色盘改小:超出去的位子作废;再改大,等着的按顺序拿空位
        let got = assign(Some(&p), &many, 3);
        assert_eq!(got.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["Codex y", "Claude home", "Codex x"]);
        let got = assign(Some(&p), &many, 6);
        std::fs::remove_file(&p).unwrap();
        assert_eq!(got[3..], [("a".into(), 3), ("b".into(), 4), ("c".into(), 5)]);
        assert_eq!(assign(None, &v(&["a"]), 5), vec![("a".into(), 0)]);
    }
}
