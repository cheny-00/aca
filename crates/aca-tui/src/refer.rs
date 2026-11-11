//! `aca ui --ref` (what the `[keys] ref` hotkey runs, the prefix key then b by default): pops up the list on the terminal the key was
//! pressed in; ⏎ puts the highlighted session's reference — the two lines `B` copies (see `data::reference`) — into the pane
//! you were typing in: pasted, no Enter, so you go on writing around it (see `App::insert_to`). Laid out like Ctrl+\: on the
//! right, the pane stays in sight; full screen on a window under 120 columns

use aca_core::{t, tr};

/// `args`: what's left of `aca ui --ref …` after `--client` was taken. Errors go on the status bar of that client (pressed in
/// tmux, there's no terminal to print to)
pub fn run(args: &mut Vec<String>) -> anyhow::Result<()> {
    let pane = crate::take_opt(args, "--pane");
    // Carried by the bound command (marks it as aca's, see `tmux::HOTKEY_MARK`)
    let _ = crate::take_opt(args, "--hotkey");
    if let Some(a) = args.iter().find(|a| *a != "--ref") {
        anyhow::bail!("{}", tr!("aca ui --ref 不支持 {a}", "aca ui --ref has no {a}"));
    }
    let client = crate::data::client().map(String::from);
    if let Err(e) = open(pane, client.clone()) {
        match &client {
            Some(c) => drop(aca_core::tmux::run(&["display-message", "-c", c, &e])),
            None => anyhow::bail!("{e}"),
        }
    }
    Ok(())
}

/// Pops up the list for picking. pane / client unset: this terminal's, when run inside tmux
fn open(pane: Option<String>, client: Option<String>) -> Result<(), String> {
    let tmux = |a: &[&str]| aca_core::tmux::run(a).map(|o| o.trim().to_string()).unwrap_or_default();
    let pane = pane.or_else(|| std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()));
    let client = client.or_else(|| std::env::var_os("TMUX").map(|_| tmux(&["display-message", "-p", "#{client_name}"])));
    let (Some(pane), Some(client)) = (pane, client.filter(|c| !c.is_empty())) else {
        return Err(t!("aca ui --ref 需要在 tmux 中运行", "aca ui --ref runs inside tmux").into());
    };
    let wide = tmux(&["display-message", "-p", "-c", &client, "#{client_width}"]).parse::<u32>().is_ok_and(|w| w >= 120);
    let q = aca_core::agent::sh_quote;
    let ui = format!("{} ui --client {} --insert {}", q(&aca_core::agent::self_exe()), q(&client), q(&pane));
    let mut a = vec!["display-popup", "-c", &client, "-E"];
    a.extend(if wide {
        ["-x", "R", "-y", "0", "-w", "56", "-h", "100%"].as_slice()
    } else {
        ["-B", "-w", "100%", "-h", "100%"].as_slice()
    });
    a.push(&ui);
    aca_core::tmux::run(&a).map(|_| ())
}
