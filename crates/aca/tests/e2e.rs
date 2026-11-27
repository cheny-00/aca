//! Runs it all end to end from the command line: starts real tmux sessions (with a fake agent inside), sends hooks, and checks
//! states, titles and events.
//!
//! Each test has its own `ACA_HOME` and its own tmux server (`-L`): they don't affect each other and can't touch real sessions.
//! Skipped when the machine has no tmux.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

mod discord_threads;

struct Sandbox {
    home: PathBuf,
    socket: String,
}

/// Not a single identity variable of the outer session is kept: a tmux server hands every session the environment of the moment
/// it started
fn scrub(c: &mut Command) -> &mut Command {
    for (k, _) in std::env::vars() {
        // Drop everything starting with CLAUDE: it says which config directory the outer session uses and where to get login credentials
        if k.starts_with("AGENTDECK_") || k.starts_with("CLAUDE") || k.starts_with("ACA_") || matches!(k.as_str(), "TMUX" | "TMUX_PANE") {
            c.env_remove(k);
        }
    }
    // aca calls git when it opens a worktree: don't read your global config (hooks, signing)
    c.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1");
    // The assertions below are written in Chinese (the default is English)
    c.env("ACA_LANG", "zh")
}

/// git called by the tests themselves: likewise without the global config; must succeed; returns stdout
fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let o = scrub(&mut c).arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}:{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

impl Sandbox {
    /// None = tmux isn't installed
    fn new(name: &str) -> Option<Sandbox> {
        if Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("没有 tmux,跳过");
            return None;
        }
        let home = std::env::temp_dir().join(format!("aca-e2e-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("config")).unwrap();
        std::fs::create_dir_all(home.join("proj")).unwrap();
        // The fake agent: started the Claude way (with --session-id, --settings), and does nothing itself
        std::fs::write(home.join("router.py"), include_str!("fake_router.py")).unwrap();
        std::fs::write(home.join("agent.sh"), "#!/bin/sh\nif [ -n \"$ACA_ROUTER_SHIFT\" ]; then exec python3 \"$ACA_HOME/router.py\" \"$@\"; fi\necho fake agent up\nexec sleep 600\n").unwrap();
        let cfg = format!("[tools.fake]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", home.join("agent.sh").display());
        std::fs::write(home.join("config/config.toml"), cfg).unwrap();
        Some(Sandbox { socket: format!("aca-e2e-{}-{name}", std::process::id()), home })
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_aca"));
        scrub(&mut c).env("ACA_HOME", &self.home).env("ACA_TMUX_SOCKET", &self.socket);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    /// Run one command; must succeed; returns stdout
    fn aca(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(o.status.success(), "aca {args:?} 失败:{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.aca(args)).unwrap()
    }

    /// An event arrived from the agent side: as for real, the JSON goes to stdin and the session is read from environment variables
    fn hook(&self, id: &str, pane: &str, payload: &str) {
        self.hook_as("claude", id, pane, payload)
    }

    fn hook_as(&self, tool: &str, id: &str, pane: &str, payload: &str) {
        use std::io::Write;
        let mut child = self
            .cmd()
            .args(["hook", tool])
            .env("ACA_SESSION_ID", id)
            .env("TMUX_PANE", pane)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
    }

    fn tmux(&self, args: &[&str]) -> String {
        let mut c = Command::new("tmux");
        let o = scrub(&mut c).args(["-u", "-L", &self.socket]).args(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn proj(&self) -> String {
        self.home.join("proj").to_string_lossy().into_owned()
    }

    /// Wait until this text appears on the pane (the fake agent takes a moment to start); returns the whole screen
    fn wait_screen(&self, pane: &str, want: &str) -> String {
        let mut screen = String::new();
        for _ in 0..50 {
            screen = self.tmux(&["capture-pane", "-p", "-t", pane]);
            if screen.contains(want) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        screen
    }

    fn event_names(&self) -> Vec<String> {
        self.aca(&["events", "-n", "100"])
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["event"].as_str().unwrap().to_string())
            .collect()
    }

    /// Two fake agents whose screens are driven by files: `scr` is started the Claude way, `fcx` the Codex way. Whatever is written
    /// in `screens/<SESSION-ID>` is what its pane shows (running, waiting for you to pick, a draft in the input box… are all staged
    /// this way); when called as `codex queue`, it records the arguments it got in queued.txt, one per line
    fn screen_tools(&self) {
        let home = self.home.display();
        write(
            &self.home.join("scr.sh"),
            &format!(
                "#!/bin/sh\n\
                 for a in \"$@\"; do [ \"$a\" = queue ] && {{ printf '%s\\n' \"$@\" '----' >> '{home}/queued.txt'; echo 'Queued message.'; exit 0; }}; done\n\
                 f='{home}/screens/'\"$ACA_SESSION_ID\"\n\
                 last=''\n\
                 while :; do now=$(cat \"$f\" 2>/dev/null); [ \"$now\" != \"$last\" ] && {{ printf '\\033[H\\033[2J%s\\n' \"$now\"; last=$now; }}; sleep 0.1; done\n"
            ),
        );
        let cfg = self.home.join("config/config.toml");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        let script = self.home.join("scr.sh");
        text += &format!(
            "[tools.scr]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{0}\"]\n[tools.fcx]\nkind = \"codex\"\ncmd = [\"/bin/sh\", \"{0}\"]\n",
            script.display()
        );
        write(&cfg, &text);
    }

    /// Stage a session's screen; when it isn't empty, wait for it to be drawn
    fn set_screen(&self, id: &str, pane: &str, text: &str) {
        write(&self.home.join("screens").join(id), text);
        // What should appear on screen (without colors): the first few characters of the first line, colors stripped
        let mut plain = String::new();
        let mut cs = text.lines().next().unwrap_or("").chars();
        while let Some(c) = cs.next() {
            if c == '\x1b' {
                cs.by_ref().find(|d| d.is_ascii_alphabetic());
            } else {
                plain.push(c);
            }
        }
        let word = plain.split_whitespace().find(|w| w.chars().any(char::is_alphanumeric)).unwrap_or("");
        if !word.is_empty() {
            let want: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
            let screen = self.wait_screen(pane, &want);
            assert!(screen.contains(&want), "屏幕没摆好:{screen}");
        }
    }

    fn pane(&self, id: &str) -> String {
        self.json(&["show", id, "--json"])["pane"].as_str().unwrap().to_string()
    }

    /// Run as a session (called from inside that session), with some text on stdin
    fn as_session(&self, me: &str, args: &[&str], input: &str) -> Output {
        use std::io::Write;
        let mut child = self
            .cmd()
            .args(args)
            .env("ACA_SESSION_ID", me)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    /// Wait until it's true, at most 8 seconds (the background delivery processes take a moment)
    fn until(&self, ok: impl Fn() -> bool) -> bool {
        for _ in 0..80 {
            if ok() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    fn queued(&self) -> String {
        std::fs::read_to_string(self.home.join("queued.txt")).unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        self.tmux(&["kill-server"]);
        // The server is gone, and tmux doesn't take the socket file. It can't be asked anymore (in tests the server may have exited long
        // ago), so build the path by tmux's rules
        if let Ok(meta) = std::fs::metadata(&self.home) {
            let tmp = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| "/tmp".into());
            let _ = std::fs::remove_file(Path::new(&tmp).join(format!("tmux-{}", meta.uid())).join(&self.socket));
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The interface's bottom bar for keys written "⏎ 打开 • s 回话" ("⏎ open • s reply"), as it reads on screen: each key a cap
/// with a cell of room either side, a space, its words, three cells to the next cap; from the first key to the last word
fn caps(keys: &str) -> String {
    let items: Vec<String> = keys
        .split(" • ")
        .map(|item| match item.split_once(' ') {
            Some((k, d)) => format!(" {k}  {d}"),
            None => format!(" {item} "),
        })
        .collect();
    items.join("   ").trim_start().to_string()
}

#[test]
fn creating_two_sessions_in_the_same_directory_without_a_locale_keeps_both_live() {
    let Some(sb) = Sandbox::new("no-locale") else { return };
    let aca = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aca"));
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &sb.home)
            .env("ACA_HOME", &sb.home)
            .env("ACA_TMUX_SOCKET", &sb.socket);
        if let Some(tmp) = std::env::var_os("TMUX_TMPDIR") {
            cmd.env("TMUX_TMPDIR", tmp);
        }
        let out = cmd.args(args).output().unwrap();
        assert!(out.status.success(), "aca {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let new = ["new", "fake", &sb.proj()];
    let first = aca(&new);
    let second = aca(&new);
    let rows: Vec<Value> = serde_json::from_str(&aca(&["ls", "--json"])).unwrap();
    assert_eq!(rows.len(), 2);
    for (id, name) in [(first, "proj"), (second, "proj (2)")] {
        let row = rows.iter().find(|r| r["id"] == id).unwrap();
        assert_eq!(row["tmux"], name);
        assert_eq!(row["state"], "idle");
    }
}

#[test]
fn a_session_is_a_tmux_session_named_by_its_title_and_stops_and_restarts_in_place() {
    let Some(sb) = Sandbox::new("life") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "修登录 v1.2", "-g", "work/app"]);
    // The session name is the title (`.` replaced), and the identity is kept in a tmux option
    assert_eq!(sb.tmux(&["list-sessions", "-F", "#{session_name}|#{@aca_id}"]), format!("修登录 v1_2|{id}"));
    let row = sb.json(&["show", &id, "--json"]);
    assert_eq!((row["state"].as_str(), row["group"].as_str(), row["tmux"].as_str()), (Some("idle"), Some("work/app"), Some("修登录 v1_2")));
    // A tool that keeps no record of where it is: it works where it was registered
    assert_eq!(row["work"], row["dir"]);
    let pane = row["pane"].as_str().unwrap().to_string();

    // Rename: the tmux session name and the window name change together; the first few characters of the id or the title both find it
    sb.aca(&["rename", &id[..4], "登录页改版"]);
    assert_eq!(sb.tmux(&["list-windows", "-a", "-F", "#{session_name}|#{window_name}"]), "登录页改版|登录页改版");
    assert_eq!(sb.json(&["show", "登录页改版", "--json"])["id"], id.as_str());

    // Stop: the agent is gone, the pane and the session stay; starting again uses the same pane
    sb.aca(&["stop", &id]);
    assert_eq!(sb.json(&["show", &id, "--json"])["state"], "exited");
    assert_eq!(sb.tmux(&["list-panes", "-a", "-F", "#{pane_id}|#{pane_dead}"]), format!("{pane}|1"));
    sb.aca(&["start", &id]);
    assert_eq!(sb.tmux(&["list-panes", "-a", "-F", "#{pane_id}|#{pane_dead}"]), format!("{pane}|0"));

    // tmux entirely gone (as after a reboot): the registration is still there; starting again creates it anew
    sb.tmux(&["kill-server"]);
    assert_eq!(sb.json(&["show", &id, "--json"])["state"], "stopped");
    sb.aca(&["start", &id]);
    assert_eq!(sb.tmux(&["list-sessions", "-F", "#{session_name}|#{@aca_id}"]), format!("登录页改版|{id}"));

    sb.aca(&["rm", &id]);
    assert_eq!(sb.tmux(&["list-sessions", "-F", "#{session_name}"]), "");
    assert_eq!(sb.json(&["ls", "--json"]), serde_json::json!([]));
}

/// A group names one of its own sessions as its router, for as long as that session is in it. Sessions move in a batch, with
/// what they sent out when asked, and the event says where each came from and whose command moved it
#[test]
fn a_group_names_its_router_and_sessions_move_in_a_batch_with_what_they_sent_out() {
    let Some(sb) = Sandbox::new("router") else { return };
    let new = |title: &str, group: &str, parent: &str| {
        let dir = sb.proj();
        let mut args = vec!["new", "fake", dir.as_str(), "--hold", "-t", title, "-g", group];
        if !parent.is_empty() {
            args.extend(["--parent", parent]);
        }
        sb.aca(&args)
    };
    let router = new("nuc router", "nuc", "");
    let lead = new("电压", "scratch", "");
    let kid = new("电压-测量", "scratch", &lead);
    let grandkid = new("电压-测量-画图", "scratch", &kid);
    let elsewhere = new("电压-别处", "other", &lead);
    let alone = new("功耗", "scratch", "");
    let stays = new("墨水屏", "scratch", "");
    let group = |id: &str| sb.json(&["show", id, "--json"])["group"].as_str().unwrap().to_string();

    // No router yet: nothing printed, exit 1. Only one of the group's own sessions can be named
    let none = sb.run(&["group", "router", "nuc"]);
    assert_eq!((none.status.code(), String::from_utf8_lossy(&none.stdout).trim()), (Some(1), ""));
    assert!(!sb.run(&["group", "router", "nuc", &lead]).status.success());
    assert!(!sb.run(&["group", "router", "nowhere"]).status.success());
    sb.aca(&["group", "router", "nuc", &router]);
    assert_eq!(sb.aca(&["group", "router", "nuc"]), router);
    let listed = sb.json(&["group", "ls", "--json"]);
    let of = |g: &str| listed.as_array().unwrap().iter().find(|x| x["path"] == g).unwrap()["router"].clone();
    assert_eq!((of("nuc"), of("scratch")), (Value::from(router.as_str()), Value::Null));
    assert!(sb.aca(&["group", "ls"]).lines().any(|l| l.contains("nuc") && l.ends_with(&format!("@{router}"))));

    // The router pulls two sessions over, with what they sent out: the ones in the same group follow however deep, the one put
    // elsewhere stays there, and who sent out whom is as it was
    let out = sb.as_session(&router, &["move", "--tree", &lead, &alone, "nuc"], "");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    for id in [&lead, &kid, &grandkid, &alone] {
        assert_eq!(group(id), "nuc");
    }
    assert_eq!((group(&elsewhere), group(&stays)), ("other".to_string(), "scratch".to_string()));
    assert_eq!(sb.json(&["show", &grandkid, "--json"])["parent"], kid.as_str());
    let moved: Vec<Value> = sb
        .aca(&["events", "-n", "100"])
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|e| e["event"] == "session.moved")
        .collect();
    let ids: Vec<&str> = moved.iter().map(|e| e["session"].as_str().unwrap()).collect();
    assert_eq!(ids, [lead.as_str(), kid.as_str(), grandkid.as_str(), alone.as_str()]);
    assert!(moved.iter().all(|e| e["group"] == "nuc" && e["from"] == "scratch" && e["by"] == router.as_str()), "{moved:?}");

    // Without --tree only the one named moves; one already there stays put and nothing is said about it; moved by you = no `by`
    sb.aca(&["move", &kid, &alone, "scratch"]);
    assert_eq!((group(&kid), group(&grandkid), group(&alone)), ("scratch".to_string(), "nuc".to_string(), "scratch".to_string()));
    sb.aca(&["move", &kid, "scratch"]);
    let all = sb.aca(&["events", "-n", "100"]);
    let last: Vec<Value> =
        all.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).filter(|e| e["event"] == "session.moved").skip(4).collect();
    assert_eq!(last.len(), 2, "{last:?}");
    assert!(last.iter().all(|e| e["from"] == "nuc" && e["by"].is_null()));
    // A key that finds no session moves nothing at all
    assert!(!sb.run(&["move", &stays, "no-such-session", "nuc"]).status.success());
    assert_eq!(group(&stays), "scratch");

    // The router moved away is no group's router; back, it is this one's again. Deleted, the group has none
    sb.aca(&["move", &router, "other"]);
    assert_eq!(sb.run(&["group", "router", "nuc"]).status.code(), Some(1));
    assert_eq!(sb.run(&["group", "router", "other"]).status.code(), Some(1));
    sb.aca(&["move", &router, "nuc"]);
    assert_eq!(sb.aca(&["group", "router", "nuc"]), router);
    sb.aca(&["group", "router", "nuc", ""]);
    assert_eq!(sb.run(&["group", "router", "nuc"]).status.code(), Some(1));
    sb.aca(&["group", "router", "nuc", &router]);
    sb.aca(&["rm", &router]);
    assert_eq!(sb.run(&["group", "router", "nuc"]).status.code(), Some(1));
}

/// Task tickets from the command line: you and a group's router make them and hand them over, the session doing one marks
/// it done, and word is passed each time — to the session handed the ticket, and to the router when one is made with nobody
/// on it or is done. A session started for a ticket has it from the start, in the ticket's group
#[test]
fn tickets_are_made_and_handed_over_by_you_and_routers_and_marked_done_by_the_session_doing_them() {
    let Some(sb) = Sandbox::new("task") else { return };
    let new = |title: &str, group: &str| {
        let dir = sb.proj();
        sb.aca(&["new", "fake", dir.as_str(), "--hold", "-t", title, "-g", group, "--parent", "-"])
    };
    let router = new("probe router", "probe");
    let (w1, w2) = (new("fix-alert", "probe"), new("fix-panel", "probe"));
    sb.aca(&["group", "router", "probe", &router]);
    let said = |o: Output| {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let refused = |o: Output, why: &str| {
        let err = String::from_utf8_lossy(&o.stderr).to_string();
        assert!(!o.status.success() && err.contains(why), "{err}");
    };
    // What a session was told, oldest first: (who by, the words)
    let told = |id: &str| -> Vec<(String, String)> {
        let v = sb.json(&["inbox", id, "--json"]);
        v.as_array().unwrap().iter().map(|m| (m["from"].as_str().unwrap().into(), m["text"].as_str().unwrap().into())).collect()
    };
    let task = |n: &str| sb.json(&["task", "show", n, "--json"]);

    // Made by you with nobody on it: only its number is printed, and the group's router hears of it
    assert_eq!(sb.aca(&["task", "add", "-g", "probe", "-t", "告警规则上测试集群", "--criteria", "造一次延迟,告警响"]), "#1");
    assert_eq!(told(&router), [("you".to_string(), "#1 新任务:告警规则上测试集群\n完成标准:造一次延迟,告警响\n还没有会话在做。".to_string())]);
    assert_eq!((task("1")["state"].as_str(), task("#1")["by"].as_str()), (Some("todo"), Some("")));
    // A session that is no router makes none and hands none over; the router does both, in its own group by default
    refused(sb.as_session(&w1, &["task", "add", "-t", "x"], ""), "router");
    refused(sb.as_session(&w1, &["task", "give", "1", &w1], ""), "router");
    assert_eq!(said(sb.as_session(&router, &["task", "add", "-t", "面板加三张图", "--note", "等 #1:改同一个文件"], "")), "#2");
    assert_eq!((task("2")["group"].as_str(), task("2")["by"].as_str()), (Some("probe"), Some(router.as_str())));
    let line = said(sb.as_session(&router, &["task", "give", "1", &w1, "这块你昨天改过"], ""));
    assert!(line.starts_with("#1  ● 告警规则上测试集群  probe  ·  fix-alert"), "{line}");
    let want = "任务 #1:告警规则上测试集群\n完成标准:造一次延迟,告警响\n这块你昨天改过\n做完后运行:aca task done 1 \"一句话说结果\"";
    assert_eq!(told(&w1), [(router.clone(), want.to_string())]);
    let t = task("1");
    assert_eq!((t["state"].as_str(), t["session"].as_str(), t["given_by"].as_str()), (Some("doing"), Some(w1.as_str()), Some(router.as_str())));

    // The list: the ones not done, every group's or one's; its line on a ticket is the router's to write, and the doer's own
    let ls = sb.aca(&["task", "ls"]);
    assert!(ls.lines().count() == 2 && ls.contains("#2  · 面板加三张图  probe"), "{ls}");
    assert_eq!(sb.aca(&["task", "ls", "-g", "probe"]).lines().count(), 2);
    assert!(!sb.run(&["task", "ls", "-g", "nope"]).status.success());
    said(sb.as_session(&w1, &["task", "set", "1", "--note", "卡在集群权限"], ""));
    refused(sb.as_session(&w2, &["task", "set", "1", "--note", "x"], ""), "router");
    refused(sb.as_session(&w1, &["task", "set", "1", "-t", "改个名"], ""), "router");
    assert_eq!(task("1")["note"], "卡在集群权限");
    assert!(sb.aca(&["task", "show", "1"]).contains("完成标准  造一次延迟,告警响"));

    // Done: not by the router, not by another session; the one doing it needs no number, and the router hears the result
    refused(sb.as_session(&router, &["task", "done", "1"], ""), "做它的会话");
    refused(sb.as_session(&w2, &["task", "done", "1", "好了"], ""), "做它的会话");
    let line = said(sb.as_session(&w1, &["task", "done", "测试集群上响了"], ""));
    assert!(line.starts_with("#1  ✓ 告警规则上测试集群"), "{line}");
    assert_eq!(told(&router).last().unwrap(), &(w1.clone(), "#1 完成:告警规则上测试集群\n测试集群上响了".to_string()));
    assert_eq!((task("1")["state"].as_str(), task("1")["result"].as_str()), (Some("done"), Some("测试集群上响了")));
    assert_eq!(sb.aca(&["task", "ls"]).lines().count(), 1);
    assert_eq!(sb.json(&["task", "ls", "--all", "--json"]).as_array().unwrap().len(), 2);

    // A session started for a ticket: it's the ticket's doer at once, in the ticket's group, with nothing sent after
    let dir = sb.proj();
    let w3 = said(sb.as_session(&router, &["new", "fake", &dir, "--hold", "--parent", "-", "-t", "fix-panel-2", "--task", "2", "--say", "照这个做"], ""));
    let t = task("2");
    assert_eq!((t["state"].as_str(), t["session"].as_str(), t["say"].as_str()), (Some("doing"), Some(w3.as_str()), Some("照这个做")));
    assert_eq!(sb.json(&["show", &w3, "--json"])["group"], "probe");
    assert!(told(&w3).is_empty());
    refused(sb.as_session(&w1, &["new", "fake", &dir, "--hold", "--task", "2"], ""), "router");

    // Handed on by you: the one that had it is told to stop. Taken back, it waits again; only you delete
    sb.aca(&["task", "give", "2", &w2]);
    assert_eq!(told(&w3), [("you".to_string(), "任务 #2 已交给「fix-panel」,不用再做。".to_string())]);
    sb.aca(&["task", "give", "2", "-"]);
    assert_eq!(task("2")["state"], "todo");
    refused(sb.as_session(&router, &["task", "rm", "2"], ""), "只有你");
    assert_eq!(sb.aca(&["task", "rm", "#2"]), "已删除任务 #2「面板加三张图」");
    assert!(!sb.run(&["task", "show", "2"]).status.success());
    // The next one doesn't take the deleted one's number; what happened is all in the events
    assert_eq!(sb.aca(&["task", "add", "-g", "probe", "-t", "下一张", "--to", &w2]), "#3");
    let names: Vec<String> = sb.event_names().into_iter().filter(|n| n.starts_with("task.")).collect();
    let want = ["added", "added", "given", "changed", "done", "given", "given", "given", "removed", "added", "given"];
    assert_eq!(names, want.map(|n| format!("task.{n}")));
}

/// The tasks page in the real interface: a group with a ticket to finish opens on its tasks; n makes one, m hands one to a
/// session (which is sent the ticket), c marks it done, d deletes one — each in the registry `aca task` reads, and each
/// back on the page without a key pressed
#[test]
fn the_tasks_page_makes_hands_over_finishes_and_deletes_tickets_in_the_real_interface() {
    let Some(sb) = Sandbox::new("task-ui") else { return };
    let dir = sb.proj();
    let w1 = sb.aca(&["new", "fake", &dir, "--hold", "-g", "probe", "-t", "fix-alert", "--parent", "-"]);
    assert_eq!(sb.aca(&["task", "add", "-g", "probe", "-t", "告警规则上测试集群", "--criteria", "告警响"]), "#1");
    let task = |n: &str| sb.json(&["task", "show", n, "--json"]);
    let ui = Ui::open_sized(&sb, "", 150, 40);
    // Into probe from the rail: its tasks page, with the ticket under the cursor beside it
    ui.keys(&["h", "j", "l"]);
    let on_page = || {
        let s = ui.screen();
        s.contains("任务 1   会话 ") && s.contains("· 告警规则上测试集群  #1") && s.contains("#1 · 待办") && s.contains("完成标准  告警响")
    };
    assert!(sb.until(on_page), "{}", ui.screen());
    assert!(ui.screen().contains(&caps("m 交给 • c 完成 • n 新任务")), "{}", ui.screen());
    // n: its title, then what counts as done; it's yours, and nobody's yet
    ui.keys(&["n", "text:面板加三张图", "Enter"]);
    assert!(sb.until(|| ui.screen().contains("新任务 · 面板加三张图 · 完成标准")), "{}", ui.screen());
    ui.keys(&["text:三张都有数据", "Enter"]);
    assert!(sb.until(|| ui.screen().contains("· 面板加三张图  #2") && ui.screen().contains("任务 2   会话 ")), "{}", ui.screen());
    let t = task("2");
    assert_eq!((t["criteria"].as_str(), t["by"].as_str(), t["state"].as_str()), (Some("三张都有数据"), Some(""), Some("todo")));
    // m: the group's sessions; ⏎ hands the ticket under the cursor to the one highlighted, which is sent the ticket
    ui.keys(&["m"]);
    assert!(sb.until(|| ui.screen().contains("交给 · #1") && ui.screen().contains("fix-alert")), "{}", ui.screen());
    ui.keys(&["Enter"]);
    assert!(sb.until(|| task("1")["session"] == w1.as_str()), "{}", ui.screen());
    let inbox = sb.json(&["inbox", &w1, "--json"]);
    let got = inbox.as_array().unwrap().last().unwrap().clone();
    assert_eq!(got["from"], "you");
    assert!(got["text"].as_str().unwrap().starts_with("任务 #1:告警规则上测试集群\n完成标准:告警响\n做完后运行:aca task done 1"), "{got}");
    assert!(sb.until(|| ui.screen().contains("你 → fix-alert")), "{}", ui.screen());
    // c: done; the page lists it first now, and the cursor is on the one still to finish
    ui.keys(&["c"]);
    assert!(sb.until(|| task("1")["state"] == "done"), "{}", ui.screen());
    assert!(sb.until(|| ui.screen().contains("✓ 告警规则上测试集群  #1") && ui.screen().contains("任务 1   会话 ")), "{}", ui.screen());
    // The session's row on the sessions page says which ticket it finished (it was never started: listed among the recent)
    ui.keys(&["Tab", "w"]);
    assert!(sb.until(|| ui.screen().contains("fix-alert  ✓ #1")), "{}", ui.screen());
    ui.keys(&["Tab", "Tab"]);
    // d asks; y deletes the one under the cursor (#2: the first not done)
    assert!(sb.until(|| ui.screen().contains("#2 · 待办")), "{}", ui.screen());
    ui.keys(&["d"]);
    assert!(sb.until(|| ui.screen().contains("删除任务？") && ui.screen().contains("面板加三张图  #2")), "{}", ui.screen());
    ui.keys(&["y"]);
    assert!(sb.until(|| !sb.run(&["task", "show", "2"]).status.success()), "{}", ui.screen());
    assert!(sb.until(|| !ui.screen().contains("面板加三张图")), "{}", ui.screen());
}

#[test]
fn stop_idle_stops_only_sessions_really_idle_long_enough_and_marks_them_apart() {
    let Some(sb) = Sandbox::new("idle") else { return };
    // The fake agents are started the Claude way and draw an input box on screen; draft's input box holds unsent text, and ticker
    // keeps refreshing at the bottom (like the spinner and timer while working)
    let mut cfg = std::fs::read_to_string(sb.home.join("config/config.toml")).unwrap();
    for (tool, script) in [
        ("box", "printf '────\\n❯ \\n────\\n'; exec sleep 600"),
        ("draft", "printf '────\\n❯ 写了一半的话\\n────\\n'; exec sleep 600"),
        ("ticker", "printf '────\\n❯ \\n────\\n'; while :; do printf '\\r%s' \"$(date +%s%N)\"; sleep 0.3; done"),
    ] {
        let path = sb.home.join(format!("{tool}.sh"));
        write(&path, &format!("#!/bin/sh\n{script}\n"));
        cfg.push_str(&format!("[tools.{tool}]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", path.display()));
    }
    write(&sb.home.join("config/config.toml"), &cfg);
    // All have talked (have a transcript, so starting again resumes). finish: this turn ran to the end (Stop); unfinished ones only
    // reported the start; seen: looked at;
    // record: extra lines written in the transcript (Claude scheduling itself to come back; written after the session started —
    // anything before the second of this start doesn't count); then: hooks reported after finishing
    let now_iso = || {
        let o = Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%S.000Z"]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let call = |name: &str, id: &str, input: &str| {
        format!(
            "{{\"timestamp\":\"{}\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"{name}\",\"input\":{input}}}]}}}}\n",
            now_iso()
        )
    };
    let reply = |id: &str, text: &str| {
        format!(
            "{{\"timestamp\":\"{}\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"{text}\"}}]}}}}\n",
            now_iso()
        )
    };
    let cron = |job: &str| {
        call("CronCreate", &format!("u-{job}"), "{\"cron\":\"57 8 * * *\",\"prompt\":\"看一眼\",\"recurring\":true}")
            + &reply(&format!("u-{job}"), &format!("Scheduled recurring task {job} (57 8 * * *). Next run: Oct 16 8:57am."))
    };
    let talk_with =
        |tool: &str, title: &str, conv: &str, finish: Option<&str>, seen: bool, record: &dyn Fn() -> String, then: &[(&str, &str)]| {
            let id = sb.aca(&["new", tool, &sb.proj(), "-t", title]);
            let pane = sb.json(&["show", &id, "--json"])["pane"].as_str().unwrap().to_string();
            sb.wait_screen(&pane, "❯");
            let t = sb.home.join(format!("{conv}.jsonl"));
            write(&t, &format!("{{\"type\":\"user\"}}\n{}", record()));
            let common = format!("\"session_id\":\"{conv}\",\"transcript_path\":\"{}\"", t.display());
            let hook = |ev: &str, more: &str| sb.hook(&id, &pane, &format!("{{\"hook_event_name\":\"{ev}\",{common}{more}}}"));
            hook("SessionStart", ",\"source\":\"startup\"");
            hook("UserPromptSubmit", ",\"prompt\":\"在吗\"");
            if let Some(bg) = finish {
                hook("Stop", &format!(",\"last_assistant_message\":\"在\",\"background_tasks\":{bg}"));
            }
            for (ev, more) in then {
                hook(ev, more);
            }
            if seen {
                sb.aca(&["seen", &id]);
            }
            id
        };
    let talk = |tool: &str, title: &str, conv: &str, finish: Option<&str>, seen: bool| {
        talk_with(tool, title, conv, finish, seen, &String::new, &[])
    };
    let idle = talk("box", "甲", "c1", Some("[]"), true);
    let unread = talk("box", "跑完没看", "c2", Some("[]"), false);
    let bg = talk("box", "有后台任务", "c3", Some("[{\"id\":1}]"), true);
    // Halfway through a turn: the hook only reported the start. The fake agent's screen has no spinner, so reading the screen takes
    // it as idle — even with the screen misread, it must not be stopped
    let half = talk("box", "跑到一半", "c4", None, false);
    let half_at = std::time::Instant::now();
    let draft = talk("draft", "有草稿", "c5", Some("[]"), true);
    let ticking = talk("ticker", "屏幕在刷", "c6", Some("[]"), true);
    let subs = talk("box", "子代理在写", "c7", Some("[]"), true);
    // A daily schedule it set itself, a /loop whose next run hasn't come yet: they live only in its process and are lost if it stops;
    // one set and then deleted is stopped as usual
    let cron_on = talk_with("box", "有定时", "c8", Some("[]"), true, &|| cron("aa11"), &[]);
    let looping = talk_with("box", "loop 在等", "c9", Some("[]"), true, &|| call("ScheduleWakeup", "u-w", "{\"delaySeconds\":3600}"), &[]);
    let cron_off =
        talk_with("box", "定时删掉了", "c10", Some("[]"), true, &|| cron("bb22") + &call("CronDelete", "u-d", "{\"id\":\"bb22\"}"), &[]);
    // Idle for nearly an hour, Claude compacts the conversation by itself: there's no turn, the hook state stays finished, and only
    // the start of compacting was reported; one done compacting is stopped as usual
    let auto = ",\"trigger\":\"auto\"";
    let compacting = talk_with("box", "在压缩", "c11", Some("[]"), true, &String::new, &[("PreCompact", auto)]);
    let compacted = talk_with("box", "压完了", "c12", Some("[]"), true, &String::new, &[("PreCompact", auto), ("PostCompact", auto)]);
    let show = |id: &str| sb.json(&["show", id, "--json"]);
    let state = |id: &str| show(id)["state"].as_str().unwrap().to_string();

    // Not idle long enough yet: not stopped
    assert_eq!(sb.aca(&["stop", "--idle", "1h"]), "没有空闲超过 1h 且可停止的会话");
    std::thread::sleep(std::time::Duration::from_millis(2100));
    // For the first 3 seconds after a turn reports its start, the list trusts the hook (running); after that, the one halfway through
    // looks idle on screen
    std::thread::sleep(std::time::Duration::from_millis(3200).saturating_sub(half_at.elapsed()));
    assert_eq!([state(&idle), state(&unread), state(&half), state(&draft)], ["idle", "unread", "idle", "idle"]);
    // The main thread has finished, a background subagent is still writing to its record
    write(&sb.home.join("c7/subagents/agent-a1.jsonl"), "{}\n");
    let out = sb.aca(&["stop", "--idle", "2s"]);
    let mut titles: Vec<&str> = out.lines().map(|l| l.split("  ").nth(1).unwrap_or("").split('(').next().unwrap()).collect();
    titles.sort();
    assert_eq!(titles, ["压完了", "定时删掉了", "甲"], "{out}");
    assert!(out.contains(&format!("☾ {idle}  甲(已空闲 ")) && out.contains(&format!("☾ {cron_off}  ")), "{out}");
    assert_eq!(state(&idle), "exited");
    assert_eq!(state(&compacted), "exited");
    for id in [&unread, &bg, &half, &draft, &ticking, &subs, &cron_on, &looping, &compacting] {
        assert_ne!(state(id), "exited", "{}", show(id));
        assert_eq!(show(id)["idle_stopped"], 0);
    }
    assert!(show(&idle)["idle_stopped"].as_u64().unwrap() > 0);
    // The list draws ☾; the event carries how many seconds it was idle before stopping
    assert!(sb.aca(&["ls"]).contains(&format!("☾ {idle}")));
    let ev = sb.aca(&["events", "-n", "200"]).lines().find(|l| l.contains("session.stopped")).unwrap().to_string();
    assert!(serde_json::from_str::<Value>(&ev).unwrap()["idle"].as_u64().unwrap() >= 2, "{ev}");

    // Start again: it resumes the original conversation and the mark is cleared; stopped once more by a person, it's ■
    assert_eq!(sb.aca(&["start", &idle]), "已启动,继续原对话");
    assert_eq!(show(&idle)["idle_stopped"], 0);
    sb.aca(&["stop", &idle]);
    assert!(sb.aca(&["ls"]).contains(&format!("■ {idle}")));
    // A badly written duration
    let o = sb.run(&["stop", "--idle", "3"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("30m、3h、1d、1h30m"));
}

#[test]
fn hooks_move_the_status_and_the_agents_own_title_becomes_the_session_name() {
    let Some(sb) = Sandbox::new("hooks") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj()]);
    let show = |sb: &Sandbox| sb.json(&["show", &id, "--json"]);
    let pane = show(&sb)["pane"].as_str().unwrap().to_string();
    assert_eq!(show(&sb)["title"], "proj");

    let transcript = sb.home.join("c1.jsonl");
    write(&transcript, "{\"type\":\"user\"}\n");
    let common = format!("\"session_id\":\"c1\",\"transcript_path\":\"{}\"", transcript.display());
    let hook = |ev: &str, more: &str| sb.hook(&id, &pane, &format!("{{\"hook_event_name\":\"{ev}\",{common}{more}}}"));

    hook("SessionStart", ",\"source\":\"startup\"");
    hook("UserPromptSubmit", ",\"prompt\":\"登录页 500 了\"");
    // The fake agent's screen shows nothing, so only what the hooks say is looked at here (--no-screen)
    let state = |sb: &Sandbox| sb.json(&["ls", "--json", "--no-screen"])[0]["state"].clone();
    assert_eq!(state(&sb), "running");
    hook("PermissionRequest", ",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"kubectl logs web-1\"}");
    let row = &sb.json(&["ls", "--json", "--no-screen"])[0];
    assert_eq!((row["state"].as_str(), row["ask"].as_str()), (Some("ask"), Some("Bash:kubectl logs web-1")));

    // When the turn finishes, the transcript already has a title it gave itself: the session name follows, the last reply is kept,
    // and nobody has looked yet, so it's unread
    write(&transcript, "{\"type\":\"user\"}\n{\"type\":\"ai-title\",\"aiTitle\":\"登录页 500 排查\",\"sessionId\":\"c1\"}\n");
    hook("Stop", ",\"last_assistant_message\":\"是 session 过期,已修。\",\"background_tasks\":[]");
    let row = show(&sb);
    assert_eq!(
        (row["title"].as_str(), row["tmux"].as_str(), row["conv"].as_str()),
        (Some("登录页 500 排查"), Some("登录页 500 排查"), Some("c1"))
    );
    assert_eq!(state(&sb), "unread");
    assert_eq!(sb.aca(&["output", &id]), "是 session 过期,已修。");
    sb.aca(&["seen", &id]);
    assert_eq!(state(&sb), "idle");

    // Once a person has renamed it, it no longer follows the agent's title; unlocked, it follows again
    sb.aca(&["rename", &id, "我起的名字"]);
    write(&transcript, "{\"type\":\"ai-title\",\"aiTitle\":\"换了个标题\",\"sessionId\":\"c1\"}\n");
    hook("Stop", ",\"last_assistant_message\":\"好\",\"background_tasks\":[]");
    assert_eq!(show(&sb)["tmux"], "我起的名字");
    sb.aca(&["rename", &id, "--auto"]);
    assert_eq!(show(&sb)["tmux"], "换了个标题");

    let names = sb.event_names();
    for want in ["session.created", "session.started", "turn.started", "agent.waiting", "turn.done", "session.titled", "session.renamed"] {
        assert!(names.iter().any(|n| n == want), "事件流里没有 {want}:{names:?}");
    }
}

#[test]
fn agy_changed_and_delayed_first_turn_titles_reach_the_event_stream() {
    use std::io::Write;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let Some(sb) = Sandbox::new("agy-title") else { return };
    let dir = sb.home.join(".gemini/antigravity-cli");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("conversation_summaries.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE conversation_summaries(conversation_id TEXT PRIMARY KEY, title TEXT);")
        .unwrap();
    let frame = |id: &str, conv: &str, state: &str| {
        let mut child = sb
            .cmd()
            .args(["hook-title", "agy"])
            .env("HOME", &sb.home)
            .env("ACA_SESSION_ID", id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let payload = serde_json::json!({"conversation_id":conv, "agent_state":state});
        child.stdin.take().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let id = sb.aca(&["new", "fake", &sb.proj()]);
    // Resuming work after review schedules no submit timer: only the changed frame can find the first title.
    assert_eq!(frame(&id, "busy", "reviewing"), "proj\n");
    assert_eq!(frame(&id, "busy", "working"), "proj\n");
    db.execute("INSERT INTO conversation_summaries VALUES ('busy', 'New title')", []).unwrap();
    let mut expected = sb.event_names();
    assert_eq!(frame(&id, "busy", "working"), "proj\n");
    assert_eq!(sb.event_names(), expected);
    assert_eq!(frame(&id, "busy", "tool_use"), "New title\n");
    expected.push("session.titled".into());
    assert_eq!(sb.event_names(), expected);
    let run = |id: &str| serde_json::from_slice::<Value>(&std::fs::read(sb.home.join(format!("state/run/{id}.json"))).unwrap()).unwrap();
    assert_eq!(run(&id)["auto_title"], "New title");

    db.execute("INSERT INTO conversation_summaries VALUES ('closed', 'Checkpointed title')", []).unwrap();
    drop(db);
    let closed = sb.aca(&["new", "fake", &sb.proj()]);
    assert!(!path.with_extension("db-wal").exists() && !path.with_extension("db-shm").exists());
    assert_eq!(frame(&closed, "closed", "working"), "Checkpointed title\n");
    let event = sb.json(&["events", "-n", "1"]);
    assert_eq!(event["event"], "session.titled");
    assert_eq!(event["session"], closed);
    assert_eq!(event["title"], "Checkpointed title");
    assert_eq!(run(&closed)["auto_title"], "Checkpointed title");
    assert!(!path.with_extension("db-wal").exists() && !path.with_extension("db-shm").exists());

    let db = rusqlite::Connection::open(&path).unwrap();
    let quiet = sb.aca(&["new", "fake", &sb.proj()]);
    assert_eq!(frame(&quiet, "quiet", "working"), "proj\n");
    db.execute("INSERT INTO conversation_summaries VALUES ('quiet', 'Delayed title')", []).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while run(&quiet)["auto_title"] != "Delayed title" {
        assert!(Instant::now() < deadline, "the first submit did not pick up the title");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(frame(&quiet, "quiet", "working"), "Delayed title\n");
}

/// Another agent reads a session with `aca output --turns N` / `--file`: found through the transcript the hook reported, a line
/// on where it is now first, then what both sides said (a message another Claude session sent it counts as something said to
/// it); --file writes the whole text version and says where
#[test]
fn output_turns_and_file_read_the_conversation_the_hook_pointed_at() {
    let Some(sb) = Sandbox::new("output-turns") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj()]);
    let pane = sb.json(&["show", &id, "--json"])["pane"].as_str().unwrap().to_string();
    let transcript = sb.home.join("c9.jsonl");
    let lines = [
        r#"{"type":"user","message":{"content":"登录页 500 了"},"timestamp":"2026-11-02T01:00:00Z"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"是 session 过期"}]},"timestamp":"2026-11-02T01:01:00Z"}"#,
        r#"{"type":"user","isMeta":true,"message":{"content":"Another Claude session sent a message: …"},"origin":{"kind":"peer","name":"api-worker","body":"迁移跑完了"},"timestamp":"2026-11-02T01:02:00Z"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"收到,接着改"}]},"timestamp":"2026-11-02T01:03:00Z"}"#,
    ];
    write(&transcript, &(lines.join("\n") + "\n"));
    let common = format!("\"session_id\":\"c9\",\"transcript_path\":\"{}\"", transcript.display());
    // Claude hands its hooks its own inbox socket for messages from other sessions: kept, and shown while it's there
    let sock = sb.home.join("cc-socks/4242.sock");
    write(&sock, "");
    let mut child = sb
        .cmd()
        .args(["hook", "claude"])
        .env("ACA_SESSION_ID", &id)
        .env("TMUX_PANE", &pane)
        .env("CLAUDE_CODE_MESSAGING_SOCKET", &sock)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let start = format!("{{\"hook_event_name\":\"SessionStart\",\"source\":\"startup\",{common}}}");
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), start.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    sb.hook(&id, &pane, &format!("{{\"hook_event_name\":\"Stop\",\"last_assistant_message\":\"收到,接着改\",{common}}}"));
    let show = sb.json(&["show", &id, "--json"]);
    assert_eq!((show["transcript"].as_str(), show["peer"].as_str()), (transcript.to_str(), sock.to_str()), "{show}");
    assert!(show["home"].as_str().is_some_and(|h| h.ends_with(".claude")), "{show}");
    std::fs::remove_file(&sock).unwrap();
    assert_eq!(sb.json(&["show", &id, "--json"])["peer"], "");

    let one = sb.aca(&["output", &id, "--turns", "1"]);
    let mut it = one.lines();
    let head = it.next().unwrap();
    // A tool with no account is labelled by its name; the directory is the real path it's working in
    assert!(head.starts_with(&format!("{id}「proj」· Claude·fake · 未读 · /")) && head.ends_with("/proj"), "{one}");
    assert_eq!(it.next(), Some(format!("最近 1 轮(共 2 轮;整段:aca output {id} --file)").as_str()), "{one}");
    assert!(one.contains("=== 别的会话发来 · api-worker · ") && one.contains("迁移跑完了") && one.ends_with("收到,接着改"), "{one}");
    assert!(!one.contains("登录页 500 了"), "{one}");
    let all = sb.aca(&["output", &id, "--turns", "5"]);
    assert!(all.lines().nth(1) == Some("全部 2 轮") && all.contains("登录页 500 了") && all.contains("是 session 过期"), "{all}");

    let file = sb.aca(&["output", &id, "--file"]);
    let at = file.lines().nth(1).and_then(|l| l.strip_prefix("文字版:")).and_then(|l| l.split('(').next()).unwrap();
    assert!(at.ends_with("/handoff/c9.md"), "{file}");
    let text = std::fs::read_to_string(at).unwrap();
    assert!(text.starts_with("# 「proj」的对话(文字版)") && text.contains("迁移跑完了") && text.contains("=== 它改过的文件 ==="), "{text}");

    // Without the flags it's still the last reply; a count that isn't a positive number is refused
    assert_eq!(sb.aca(&["output", &id]), "收到,接着改");
    let o = sb.run(&["output", &id, "--turns", "0"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--turns 要一个正整数"));
}

#[test]
fn messages_claude_passed_from_other_claude_sessions_land_in_the_inbox_once_with_who_sent_them() {
    let Some(sb) = Sandbox::new("native-msg") else { return };
    let spawn_hook = |id: &str, payload: &str, sock: Option<&std::path::Path>| {
        let pane = sb.json(&["show", id, "--json"])["pane"].as_str().unwrap().to_string();
        let mut c = sb.cmd();
        c.args(["hook", "claude"]).env("ACA_SESSION_ID", id).env("TMUX_PANE", &pane).stdin(std::process::Stdio::piped());
        if let Some(sock) = sock {
            c.env("CLAUDE_CODE_MESSAGING_SOCKET", sock);
        }
        let mut child = c.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), payload.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
    };
    // The sender: its Claude tells aca its inbox socket (the file name is its process id)
    let api = sb.aca(&["new", "fake", &sb.proj(), "-t", "api"]);
    let sock = sb.home.join("cc-socks/4242.sock");
    spawn_hook(&api, r#"{"hook_event_name":"SessionStart","source":"startup","session_id":"s1"}"#, Some(&sock));
    // The receiver: you said something earlier; then a message from api opened a turn, and one from a Claude aca doesn't know
    // came in mid-turn
    let web = sb.aca(&["new", "fake", &sb.proj(), "-t", "web"]);
    let transcript = sb.home.join("r1.jsonl");
    let lines = [
        r#"{"type":"user","promptId":"p1","origin":{"kind":"human"},"message":{"content":"修一下登录"},"timestamp":"2026-11-02T01:00:00.000Z"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"好了"}]},"timestamp":"2026-11-02T01:01:00.000Z"}"#,
        r#"{"type":"user","isMeta":true,"promptId":"p2","origin":{"kind":"peer","from":"uds:/tmp/cc-socks/4242.sock","verifiedPeerPid":4242,"msg_id":"bbbb2222-0000-4000-8000-000000000002","name":"api","body":"迁移跑完了"},"message":{"content":"Another Claude session sent a message: …"},"timestamp":"2099-01-01T00:00:00.000Z"}"#,
        r#"{"type":"attachment","attachment":{"type":"queued_command","prompt":"…","origin":{"kind":"peer","from":"uds:/tmp/cc-socks/999.sock","verifiedPeerPid":999,"msg_id":"9999aaaa-0000-4000-8000-000000000009","name":"outsider","body":"顺便说一声"}},"timestamp":"2099-01-01T00:00:10.000Z"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"收到"}]},"timestamp":"2099-01-01T00:00:20.000Z"}"#,
    ];
    write(&transcript, &(lines.join("\n") + "\n"));
    let common = format!("\"session_id\":\"r1\",\"transcript_path\":\"{}\"", transcript.display());
    spawn_hook(
        &web,
        &format!(
            "{{\"hook_event_name\":\"UserPromptSubmit\",\"prompt\":\"Another Claude session sent a message: …\",\"prompt_id\":\"p2\",{common}}}"
        ),
        None,
    );
    let stop = format!("{{\"hook_event_name\":\"Stop\",\"last_assistant_message\":\"收到\",{common}}}");
    spawn_hook(&web, &stop, None);
    // Taken once each, even when the next turn's end reads the same lines again
    spawn_hook(&web, &stop, None);
    let inbox = sb.json(&["inbox", &web, "--json"]);
    let got: Vec<(&str, &str, &str, &str, &str)> = inbox
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["id"].as_str().unwrap(),
                m["from"].as_str().unwrap(),
                m["via"].as_str().unwrap(),
                m["stage"].as_str().unwrap(),
                m["text"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [("cbbbb2222", api.as_str(), "claude", "got", "迁移跑完了"), ("c9999aaaa", "", "claude", "got", "顺便说一声")],
        "{inbox}"
    );
    assert!(inbox[0]["who"].as_str().unwrap().starts_with(&format!("{api}「api」")), "{inbox}");
    assert_eq!(inbox[1]["who"], "「outsider」claude");
    let text = sb.aca(&["inbox", &web]);
    assert!(text.contains("[已送达] cbbbb2222") && text.contains("说(SendMessage):迁移跑完了"), "{text}");
    // The turn was opened by api's message: what the session says you said is still yours
    assert_eq!(sb.json(&["show", &web, "--json"])["run"]["prompt"], "修一下登录");
}

#[test]
fn a_tool_declared_only_in_config_starts_reports_through_the_hook_protocol_and_resumes() {
    let Some(sb) = Sandbox::new("declared") else { return };
    // A tool with no kind: the fake agent prints on screen the arguments it got and the tool name aca put in its environment, plus
    // a "busy" line when TOY_BUSY has a value
    write(
        &sb.home.join("toy.sh"),
        "#!/bin/sh\necho \"args:[$*]\"\necho \"tool:$ACA_TOOL\"\n[ -n \"$TOY_BUSY\" ] && echo 'Working...'\nexec sleep 600\n",
    );
    let cfg = sb.home.join("config/config.toml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text += &format!(
        "[tools.toy]\ncmd = [\"/bin/sh\", \"{}\"]\nresume = [\"--session\", \"{{conv}}\"]\nbusy = ['^Working\\.\\.\\.$']\nenv = {{ TOY_BUSY = \"1\" }}\n",
        sb.home.join("toy.sh").display()
    );
    write(&cfg, &text);
    assert!(sb.aca(&["names"]).lines().any(|l| l == "toy"));

    let id = sb.aca(&["new", "toy", &sb.proj()]);
    let pane = sb.json(&["show", &id, "--json"])["pane"].as_str().unwrap().to_string();
    let screen = sb.wait_screen(&pane, "Working...");
    assert!(screen.contains("args:[]") && screen.contains("tool:toy"), "{screen}");
    // Recognized as busy from the screen (the regex in the config)
    assert_eq!(sb.json(&["show", &id, "--json"])["state"], "running");

    // The forwarding script reports events by the protocol: `aca hook <TOOL-NAME>`. The conversation id, the last reply and the
    // title it gave itself are all picked up
    let hook = |json: &str| sb.hook_as("toy", &id, &pane, json);
    hook(r#"{"hook_event_name":"SessionStart","session_id":"t-1"}"#);
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"t-1","prompt":"改一下"}"#);
    hook(r#"{"hook_event_name":"Stop","session_id":"t-1","last_assistant_message":"改好了","title":"玩具标题"}"#);
    let row = sb.json(&["ls", "--json", "--no-screen"])[0].clone();
    assert_eq!(
        (row["state"].as_str(), row["conv"].as_str(), row["title"].as_str(), row["tmux"].as_str()),
        (Some("unread"), Some("t-1"), Some("玩具标题"), Some("玩具标题"))
    );
    assert_eq!(sb.aca(&["output", &id]), "改好了");

    // Stopped and started again: resumes that conversation by the resume template
    sb.aca(&["stop", &id]);
    sb.aca(&["start", &id]);
    let screen = sb.wait_screen(&pane, "args:[--session t-1]");
    assert!(screen.contains("args:[--session t-1]"), "{screen}");
    // No fork written: it can't fork
    assert!(!sb.run(&["fork", &id]).status.success());
}

#[test]
fn an_account_is_a_layer_on_the_launch_and_run_hands_it_to_the_tools_own_commands() {
    let Some(sb) = Sandbox::new("account") else { return };
    // The fake agent prints the account layer on screen (only the last part of directories, so lines don't wrap); given login, it
    // takes it as its own subcommand and exits after printing
    write(
        &sb.home.join("toy.sh"),
        "#!/bin/sh\necho \"args:[$*]\"\necho \"home:[${TOY_HOME##*/}]\"\necho \"key:[$TOY_KEY]\"\necho \"sess:[$ACA_SESSION_ID]\"\n\
         case \"$*\" in *login*) exit 0 ;; esac\nexec sleep 600\n",
    );
    write(&sb.home.join("k.env"), "# 这个账号的 key\nexport TOY_KEY=\"k-1\"\n");
    let at = |name: &str| sb.home.join(name).to_string_lossy().into_owned();
    let cfg = sb.home.join("config/config.toml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text += &format!(
        "[tools.toy]\ncmd = [\"/bin/sh\", \"{}\"]\nhome_var = \"TOY_HOME\"\nargs = [\"--yolo\"]\n\
         [accounts.toy]\nplain = \"{}\"\nwork = {{ home = \"{}\", env_file = \"{}\", args = [\"--profile\", \"w\"] }}\n",
        at("toy.sh"),
        at("toy-plain"),
        at("toy-work"),
        at("k.env")
    );
    write(&cfg, &text);
    assert!(sb.aca(&["names"]).lines().any(|l| l == "toy:work"));
    let screen_of = |id: &str, want: &str| {
        let pane = sb.json(&["show", id, "--json"])["pane"].as_str().unwrap().to_string();
        sb.wait_screen(&pane, want)
    };

    // A registered account: its directory, the key from env_file and its arguments are all passed, the arguments after the tool's
    let work = sb.aca(&["new", "toy:work", &sb.proj()]);
    let screen = screen_of(&work, "sess:[");
    assert!(screen.contains("args:[--yolo --profile w]") && screen.contains("home:[toy-work]") && screen.contains("key:[k-1]"), "{screen}");
    // The account is passed only to the agent's pane: another window opened in the same tmux session (prefix c, other agents put
    // in later) gets none of it
    let pane = sb.json(&["show", &work, "--json"])["pane"].as_str().unwrap().to_string();
    let sid = sb.tmux(&["display-message", "-p", "-t", &pane, "#{session_id}"]);
    let side = sb.tmux(&[
        "new-window",
        "-t",
        &format!("{sid}:"),
        "-P",
        "-F",
        "#{pane_id}",
        "sh",
        "-c",
        "echo \"side:[$TOY_HOME][$TOY_KEY][$ACA_SESSION_ID]\"; exec sleep 600",
    ]);
    let screen = sb.wait_screen(&side, "side:[");
    assert!(screen.contains("side:[][][]"), "{screen}");

    // No account given: aca adds nothing
    let bare = sb.aca(&["new", "toy", &sb.proj()]);
    let screen = screen_of(&bare, "sess:[");
    assert!(screen.contains("args:[--yolo]") && screen.contains("home:[]") && screen.contains("key:[]"), "{screen}");

    // An unregistered directory used directly: the full path is recorded and the list shows the directory name; if it happens to
    // be a registered directory, it's called by that name
    let adhoc = sb.aca(&["new", &format!("toy:{}/", at("toy-new")), &sb.proj()]);
    assert!(screen_of(&adhoc, "sess:[").contains("home:[toy-new]"));
    assert_eq!(sb.json(&["show", &adhoc, "--json"])["account"], at("toy-new").as_str());
    assert!(sb.aca(&["ls", "--no-screen"]).contains("toy:toy-new"));
    let plain = sb.aca(&["new", &format!("toy:{}", at("toy-plain")), &sb.proj(), "--hold"]);
    assert_eq!(sb.json(&["show", &plain, "--json"])["account"], "plain");

    // No session created; the tool's own command runs as is, with the account: the account's environment is there, the arguments
    // are only yours, and the session identity isn't passed
    let o = sb.cmd().args(["run", "toy:work", "--", "login", "--device"]).env("ACA_SESSION_ID", &work).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "args:[login --device]\nhome:[toy-work]\nkey:[k-1]\nsess:[]\n");
    assert_eq!(sb.json(&["ls", "--json", "--no-screen"]).as_array().unwrap().len(), 4);

    // A broken config: the command line says where it's broken
    write(&cfg, "[accounts.toy]\nwork = { home = 1 }\n");
    let o = sb.run(&["names"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("config.toml"), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn a_plugin_hears_events_and_can_put_a_token_on_the_row() {
    let Some(sb) = Sandbox::new("plugin") else { return };
    let dir = sb.home.join("my-plugin");
    write(
        &dir.join("aca-plugin.toml"),
        "id = \"test.mark\"\nname = \"Mark\"\nversion = \"0.1.0\"\n\
         [[events]]\non = \"turn.done\"\ncommand = [\"sh\", \"mark.sh\"]\n\
         [[actions]]\nid = \"hello\"\ntitle = \"打招呼\"\ncontexts = [\"session\"]\ncommand = [\"sh\", \"-c\", \"echo hi $ACA_SESSION_ID from $ACA_PLUGIN_ID\"]\n",
    );
    // An event hook: it calls back into aca to attach some text to this row, then leaves a mark saying it ran
    write(
        &dir.join("mark.sh"),
        "\"$ACA_BIN\" meta set \"$ACA_SESSION_ID\" mark \"$ACA_PLUGIN_EVENT\"\ntouch \"$ACA_PLUGIN_STATE_DIR/ran\"\n",
    );
    assert!(sb.aca(&["plugin", "link", &dir.to_string_lossy()]).contains("test.mark"));

    let id = sb.aca(&["new", "fake", &sb.proj()]);
    let pane = sb.json(&["show", &id, "--json"])["pane"].as_str().unwrap().to_string();
    sb.hook(&id, &pane, "{\"hook_event_name\":\"Stop\",\"session_id\":\"c1\",\"last_assistant_message\":\"好\"}");
    // Hooks aren't waited for: give it a moment
    let ran = sb.home.join("state/plugins/test.mark/ran");
    for _ in 0..50 {
        if ran.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ran.exists(), "插件的事件钩子没跑:{}", sb.aca(&["plugin", "log", "test.mark"]));
    assert_eq!(sb.json(&["show", &id, "--json"])["tokens"]["mark"], "turn.done");

    // Actions run in the foreground and know which session they're for
    assert_eq!(sb.aca(&["plugin", "run", "test.mark.hello", "--session", &id]), format!("hi {id} from test.mark"));
    // Once disabled, it gets no more events and its actions can't be called
    sb.aca(&["plugin", "disable", "test.mark"]);
    assert!(!sb.run(&["plugin", "run", "test.mark.hello"]).status.success());
}

/// A "Claude" not started by aca: the process name is claude, it takes any arguments and just hangs around; wrapped in a
/// shell (that's what agent-deck starts look like), with its own config directory. Returns (tmux session number, pane,
/// agent pid); None = the fake program can't be compiled on this machine.
/// It must be a program really called that: the system takes the process name from the file itself, so links and scripts
/// can't fool it, and system programs copied out aren't allowed to run. So one is compiled on the spot
fn foreign_claude(sb: &Sandbox, name: Option<&str>, env: &str) -> Option<(String, String, String)> {
    let bin = sb.home.join("bin/claude");
    if !bin.exists() {
        let src = sb.home.join("bin/fake.rs");
        write(&src, "fn main() {\n    loop {\n        std::thread::sleep(std::time::Duration::from_secs(3600));\n    }\n}\n");
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        if !Command::new(rustc).arg("-o").arg(&bin).arg(&src).status().is_ok_and(|s| s.success()) {
            eprintln!("编不出假的 claude,跳过");
            return None;
        }
    }
    let cmd = format!(
        "{env} CLAUDE_CONFIG_DIR={} {} --session-id c-old --dangerously-skip-permissions; true",
        sb.home.join("cc").display(),
        bin.display()
    );
    let mut args = vec!["new-session", "-d", "-P", "-F", "#{session_id} #{pane_id} #{pane_pid}", "-c"];
    let proj = sb.proj();
    args.push(&proj);
    if let Some(n) = name {
        args.extend(["-s", n]);
    }
    args.push(&cmd);
    let out = sb.tmux(&args);
    let f: Vec<&str> = out.split(' ').collect();
    let mut pid = String::new();
    for _ in 0..50 {
        let o = Command::new("pgrep").args(["-P", f[2]]).output().unwrap();
        pid = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !pid.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    assert!(!pid.is_empty(), "假的 claude 没起来:{out}");
    Some((f[0].to_string(), f[1].to_string(), pid))
}

fn alive(pid: &str) -> bool {
    Command::new("kill").args(["-0", pid]).stderr(std::process::Stdio::null()).status().unwrap().success()
}

/// Adopting reads things under `~` (the default config directory, agent-deck's database): point HOME into the sandbox too
fn at_home(sb: &Sandbox, args: &[&str]) -> String {
    let o = sb.cmd().env("HOME", &sb.home).args(args).output().unwrap();
    assert!(o.status.success(), "aca {args:?} 失败:{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn a_running_session_aca_did_not_start_is_adopted_in_place_restarted_and_released() {
    let Some(sb) = Sandbox::new("adopt") else { return };
    // After a restart, what aca starts is that fake claude too (takes any arguments); what it started as is read from the
    // process's command line
    let cfg = sb.home.join("config/config.toml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text += &format!(
        "[tools.claude]\ncmd = [\"{}\"]\n[accounts.claude]\nsub = \"{}\"\n",
        sb.home.join("bin/claude").display(),
        sb.home.join("cc").display()
    );
    write(&cfg, &text);
    let Some((sid, pane, pid)) = foreign_claude(&sb, None, "") else { return };
    let tmux_name = sb.tmux(&["display-message", "-p", "-t", &pane, "#{session_name}"]);
    // What the real Claude records itself: which conversation it's on now, the title; the transcript is in its config directory
    write(
        &sb.home.join(format!("cc/sessions/{pid}.json")),
        &format!(r#"{{"pid":{pid},"sessionId":"c-live","cwd":"{}","name":"修登录"}}"#, sb.proj()),
    );
    write(&sb.home.join("cc/projects/p/c-live.jsonl"), "{\"type\":\"user\"}\n");
    let aca = |args: &[&str]| at_home(&sb, args);
    let ls = || serde_json::from_str::<Value>(&aca(&["ls", "--json"])).unwrap();
    let panes = || sb.tmux(&["list-panes", "-a", "-F", "#{session_name}|#{@aca_id}|#{@aca_agent}|#{remain-on-exit}|#{pane_dead}"]);

    // No arguments: list them to look at, change nothing. Which harness, which account, which conversation, which arguments to
    // carry over were recognized
    let listed = aca(&["adopt"]);
    assert!(
        listed.contains("「修登录」 claude:sub")
            && listed.contains("对话 c-live")
            && listed.contains("参数 --dangerously-skip-permissions"),
        "{listed}"
    );
    assert!(aca(&["adopt", &sid, "-n"]).contains("将接管"));
    assert_eq!(ls(), serde_json::json!([]));
    assert_eq!(panes(), format!("{tmux_name}|||off|0"));

    // Adopt: registered, the tmux session renamed to the title, the agent's pane recorded; the process is the original one,
    // untouched
    let said = aca(&["adopt", &tmux_name, "-g", "work"]);
    assert!(said.contains("已接管「修登录」") && said.contains(&format!("{tmux_name} → 修登录")), "{said}");
    let row = ls()[0].clone();
    let id = row["id"].as_str().unwrap().to_string();
    assert_eq!(
        (
            row["title"].as_str(),
            row["tool"].as_str(),
            row["account"].as_str(),
            row["group"].as_str(),
            row["conv"].as_str(),
            row["state"].as_str()
        ),
        (Some("修登录"), Some("claude"), Some("sub"), Some("work"), Some("c-live"), Some("idle"))
    );
    assert_eq!(row["pane"].as_str(), Some(pane.as_str()));
    assert_eq!(panes(), format!("修登录|{id}|1|on|0"));
    // The carried-over arguments don't include the two words for "which conversation to resume"
    assert!(aca(&["show", &id, "--json"]).contains("\"args\":[\"--dangerously-skip-permissions\"]"));
    assert!(alive(&pid), "接管不该动那个进程");
    // Adopting again: it's already aca's
    let again = sb.cmd().env("HOME", &sb.home).args(["adopt", "修登录"]).output().unwrap();
    assert!(!again.status.success() && String::from_utf8_lossy(&again.stderr).contains("已是 aca 会话"));

    // Restart: the original process exits cleanly (it isn't the pane's root process; there's a shell around it), and aca starts
    // it in the same pane: resuming that conversation,
    // with hooks, and the carried-over arguments still there
    aca(&["restart", &id]);
    assert!(!alive(&pid), "原来的进程还在");
    assert_eq!(sb.tmux(&["list-panes", "-a", "-F", "#{pane_id}|#{pane_dead}"]), format!("{pane}|0"));
    let now = sb.tmux(&["display-message", "-p", "-t", &pane, "#{pane_pid}"]);
    let o = Command::new("ps").args(["-ww", "-o", "args=", "-p", &now]).output().unwrap();
    let line = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert!(
        line.contains("claude --resume c-live --settings ") && line.ends_with("hooks/claude.json --dangerously-skip-permissions"),
        "{line}"
    );

    // Release: the registration is gone, the tmux session and the process in it stay, back to an ordinary session
    // (remain-on-exit is taken back too)
    assert!(aca(&["release", &id]).contains("已将「修登录」移出 aca"));
    assert_eq!(ls(), serde_json::json!([]));
    assert_eq!(panes(), "修登录|||off|0");
    assert!(alive(&now));

    // Adopt it back: this process was started by aca and its hooks still report to the original id — keep that id; the account
    // is recognized from its environment;
    // what aca itself passed in (--resume, its --settings) isn't taken as arguments to carry over
    write(&sb.home.join(format!("cc/sessions/{now}.json")), &format!(r#"{{"pid":{now},"sessionId":"c-live","cwd":"{}"}}"#, sb.proj()));
    let said = aca(&["adopt", &pane]);
    assert!(said.contains(&format!("已接管「修登录」 {id}")) && !said.contains("重启前"), "{said}");
    let row = ls()[0].clone();
    assert_eq!((row["id"].as_str(), row["account"].as_str(), row["conv"].as_str()), (Some(id.as_str()), Some("sub"), Some("c-live")));
    assert!(aca(&["show", &id, "--json"]).contains("\"args\":[\"--dangerously-skip-permissions\"]"));

    // A program it can't recognize (a sleep): registered as --as says; title, conversation and arguments given by hand
    let other = sb.tmux(&["new-session", "-d", "-P", "-F", "#{pane_id}", "-s", "杂项", "-c", &sb.proj(), "sleep 600"]);
    assert!(aca(&["adopt", "杂项", "-n"]).contains("shell"));
    let said = aca(&["adopt", &other, "--as", "fake:sub", "-t", "又一个", "--conv", "c-9", "--", "--mine"]);
    assert!(said.contains("已接管「又一个」"), "{said}");
    let row = serde_json::from_str::<Value>(&aca(&["show", "又一个", "--json"])).unwrap();
    assert_eq!(
        (row["tool"].as_str(), row["account"].as_str(), row["conv"].as_str(), row["locked"].as_bool(), row["tmux"].as_str()),
        (Some("fake"), Some("sub"), Some("c-9"), Some(true), Some("又一个"))
    );
    assert_eq!(row["args"], serde_json::json!(["--mine"]));
    let names = sb.event_names();
    assert!(names.iter().filter(|n| *n == "session.created").count() == 3 && names.iter().any(|n| n == "session.removed"), "{names:?}");
}

#[test]
fn the_title_an_agent_writes_on_its_pane_is_followed_without_any_hook() {
    let Some(sb) = Sandbox::new("pane-title") else { return };
    let Some((_, pane, pid)) = foreign_claude(&sb, None, "") else { return };
    // When the real Claude hasn't named it, it makes up a placeholder itself (nameSource is derived): that isn't a title. The
    // title it writes on the pane title
    write(
        &sb.home.join(format!("cc/sessions/{pid}.json")),
        &format!(r#"{{"pid":{pid},"sessionId":"c-live","cwd":"{}","name":"proj-5e","nameSource":"derived"}}"#, sb.proj()),
    );
    let on_pane = |pane: &str, title: &str| sb.tmux(&["select-pane", "-t", pane, "-T", title]);
    on_pane(&pane, "✳ 登录页 500 排查");
    let aca = |args: &[&str]| at_home(&sb, args);
    let said = aca(&["adopt", &pane]);
    assert!(said.contains("已接管「登录页 500 排查」"), "{said}");
    let row = |id: &str| {
        let all = serde_json::from_str::<Value>(&aca(&["ls", "--json", "--no-screen"])).unwrap();
        all.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone()
    };
    let names = |id: &str| {
        let r = row(id);
        (r["title"].as_str().unwrap().to_string(), r["tmux"].as_str().unwrap().to_string())
    };
    let id = serde_json::from_str::<Value>(&aca(&["ls", "--json", "--no-screen"])).unwrap()[0]["id"].as_str().unwrap().to_string();

    // Adopted and not restarted, so no hooks: it changed its title, and one listing catches up, the tmux session name too
    on_pane(&pane, "✳ 换了个标题");
    assert_eq!(names(&id), ("换了个标题".into(), "换了个标题".into()));
    // When it starts a new conversation it writes back the "no title yet" one: doesn't count, the original is kept
    on_pane(&pane, "✳ Claude Code");
    assert_eq!(names(&id).0, "换了个标题");
    // One renamed by a person doesn't follow; unlocked, it follows again
    aca(&["rename", &id, "我起的名字"]);
    on_pane(&pane, "◐ 又换了");
    assert_eq!(names(&id), ("我起的名字".into(), "我起的名字".into()));
    aca(&["rename", &id, "--auto"]);
    assert_eq!(names(&id), ("又换了".into(), "又换了".into()));

    // The same for one aca started itself: this turn hasn't finished, the title is already on the pane, no need to wait for the
    // finishing event
    let own = aca(&["new", "fake", &sb.proj()]);
    let own_pane = row(&own)["pane"].as_str().unwrap().to_string();
    assert_eq!(names(&own).0, "proj");
    on_pane(&own_pane, "✳ 第一轮就有的标题");
    assert_eq!(names(&own), ("第一轮就有的标题".into(), "第一轮就有的标题".into()));
    // When the turn finishes, the transcript has a different one: go by the pane (that's what it shows itself), no flip-flopping
    let transcript = sb.home.join("c1.jsonl");
    write(&transcript, "{\"type\":\"ai-title\",\"aiTitle\":\"记录里的\",\"sessionId\":\"c1\"}\n");
    sb.hook(
        &own,
        &own_pane,
        &format!("{{\"hook_event_name\":\"Stop\",\"session_id\":\"c1\",\"transcript_path\":\"{}\"}}", transcript.display()),
    );
    assert_eq!(names(&own), ("第一轮就有的标题".into(), "第一轮就有的标题".into()));
    // Events are sent only for the two times it really followed: placeholder, locked and unchanged ones send none
    let titled = sb.event_names().iter().filter(|n| *n == "session.titled").count();
    assert_eq!(titled, 2, "{:?}", sb.event_names());
}

#[test]
fn a_session_agent_deck_started_keeps_its_title_group_and_id_when_adopted() {
    let Some(sb) = Sandbox::new("adopt-deck") else { return };
    if Command::new("sqlite3").arg("-version").output().is_err() {
        eprintln!("没有 sqlite3,跳过");
        return;
    }
    // agent-deck's database: only the few columns adopting reads
    let db = sb.home.join(".local/share/agent-deck/profiles/default/state.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let sql = "create table instances(id text, title text, title_locked integer, group_path text, sort_order integer, tool text, account text, \
               project_path text, tool_data text, tmux_session text, archived_at integer, parent_session_id text, created_at integer); \
               insert into instances values('ad-77', '架构替换方案', 0, 'runtime', 4, 'claude', 'sub', '/p', \
               '{\"claude_session_id\":\"c-deck\"}', 'agentdeck_iron-dune_92cb', 0, '', 1790000000);";
    assert!(Command::new("sqlite3").arg(&db).arg(sql).status().unwrap().success());
    // A session it started: the session name is one it made up, and the process environment carries its id. This fake left no
    // "which conversation is it on now" file: go by the one on the command line
    let Some((_, pane, pid)) = foreign_claude(&sb, Some("agentdeck_iron-dune_92cb"), "AGENTDECK_INSTANCE_ID=ad-77") else { return };
    let aca = |args: &[&str]| at_home(&sb, args);

    // Part of the name is enough
    let said = aca(&["adopt", "iron-dune"]);
    assert!(said.contains("已接管「架构替换方案」 ad-77") && said.contains("agent-deck -p default remove ad-77"), "{said}");
    let row = serde_json::from_str::<Value>(&aca(&["show", "ad-77", "--json"])).unwrap();
    assert_eq!(
        (row["title"].as_str(), row["group"].as_str(), row["conv"].as_str(), row["tmux"].as_str(), row["created"].as_u64()),
        (Some("架构替换方案"), Some("runtime"), Some("c-old"), Some("架构替换方案"), Some(1_790_000_000))
    );
    // An unregistered config directory: the account records that directory directly
    assert_eq!(row["account"].as_str(), sb.home.join("cc").to_str());
    // The database is read-only: the records there are unchanged
    let o = Command::new("sqlite3").arg(&db).arg("select tmux_session from instances").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "agentdeck_iron-dune_92cb");

    // Before the restart it doesn't carry aca's hooks, but carries agent-deck's, which keep reporting: borrow what that side
    // records, so "running" and "unread" are there without waiting for a restart
    let hook = sb.home.join(".local/share/agent-deck/hooks/ad-77.json");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    let state = || serde_json::from_str::<Value>(&aca(&["ls", "--json", "--no-screen"])).unwrap()[0]["state"].clone();
    assert_eq!(state(), "idle");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    std::fs::write(&hook, format!(r#"{{"status":"waiting","session_id":"c-old","event":"Stop","ts":{now}}}"#)).unwrap();
    assert_eq!(state(), "unread");
    aca(&["seen", "ad-77"]);
    assert_eq!(state(), "idle");
    std::fs::write(&hook, format!(r#"{{"status":"running","session_id":"c-old","event":"UserPromptSubmit","ts":{now}}}"#)).unwrap();
    assert_eq!(state(), "running");

    // Stop: the agent inside and the shell around it both exit, the pane stays
    aca(&["stop", "ad-77"]);
    assert_eq!(sb.tmux(&["list-panes", "-a", "-F", "#{pane_id}|#{pane_dead}"]), format!("{pane}|1"));
    assert!(!alive(&pid));
}

#[test]
fn a_session_imported_while_running_is_attached_to_its_registration_and_the_rest_come_with_all() {
    let Some(sb) = Sandbox::new("adopt-known") else { return };
    if Command::new("sqlite3").arg("-version").output().is_err() {
        eprintln!("没有 sqlite3,跳过");
        return;
    }
    let cfg = sb.home.join("config/config.toml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text += &format!("[accounts.claude]\nsub = \"{}\"\n", sb.home.join("cc").display());
    write(&cfg, &text);
    let db = sb.home.join(".local/share/agent-deck/profiles/default/state.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let sql = "create table groups(path text, sort_order integer, default_path text); insert into groups values('g', 1, ''); \
               create table instances(id text, title text, title_locked integer, group_path text, sort_order integer, tool text, account text, \
               project_path text, tool_data text, tmux_session text, archived_at integer, parent_session_id text, created_at integer); \
               insert into instances values('ad-5', '导入过的', 1, 'g', 2, 'claude', 'sub', '/p', \
               '{\"claude_session_id\":\"c-deck\"}', 'agentdeck_k_5', 0, '', 1790000000);";
    assert!(Command::new("sqlite3").arg(&db).arg(sql).status().unwrap().success());
    let aca = |args: &[&str]| at_home(&sb, args);
    let ls = || serde_json::from_str::<Value>(&aca(&["ls", "--json"])).unwrap();

    // First move all of them over in one batch (the running one registered too): registered, and stopped as far as aca can tell
    assert!(aca(&["import", "agent-deck", "--apply", "--all"]).contains("已导入 1 个"));
    assert_eq!(ls()[0]["state"], "stopped");
    let Some((_, pane, _)) = foreign_claude(&sb, Some("agentdeck_k_5"), "AGENTDECK_INSTANCE_ID=ad-5") else { return };

    // Adopt: no new registration; this tmux session is attached to that one. Conversation and arguments follow what's on the
    // process now, the rest follows the registration
    let said = aca(&["adopt", "k_5"]);
    assert!(said.contains("已接管「导入过的」 ad-5"), "{said}");
    let rows = ls();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(
        (rows[0]["id"].as_str(), rows[0]["state"].as_str(), rows[0]["conv"].as_str(), rows[0]["tmux"].as_str(), rows[0]["pane"].as_str()),
        (Some("ad-5"), Some("idle"), Some("c-old"), Some("导入过的"), Some(pane.as_str()))
    );
    assert_eq!(
        serde_json::from_str::<Value>(&aca(&["show", "ad-5", "--json"])).unwrap()["args"],
        serde_json::json!(["--dangerously-skip-permissions"])
    );

    // Two more unregistered ones: --all adopts only the one with a recognizable agent, and doesn't adopt again what's already aca's
    foreign_claude(&sb, None, "").unwrap();
    sb.tmux(&["new-session", "-d", "-s", "只是个壳", "-c", &sb.proj(), "sleep 600"]);
    let listed = aca(&["adopt"]);
    assert!(
        listed.contains("另有 1 个 tmux 会话未识别到 agent") && listed.contains("只是个壳") && !listed.contains("导入过的"),
        "{listed}"
    );
    let said = aca(&["adopt", "--all", "-g", "批量"]);
    assert_eq!(said.matches("已接管").count(), 1, "{said}");
    let rows = ls();
    let groups: Vec<&str> = rows.as_array().unwrap().iter().map(|r| r["group"].as_str().unwrap()).collect();
    assert_eq!(groups, ["g", "批量"]);
    assert_eq!(aca(&["adopt", "--all"]), "没有可直接接管的 tmux 会话");
}

#[test]
fn a_dir_becomes_a_group_named_after_it_and_the_group_can_be_renamed_and_rehomed() {
    let Some(sb) = Sandbox::new("groups") else { return };
    let dir = |p: &str| {
        let d = sb.home.join(p);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap().to_string_lossy().into_owned()
    };
    let (a, b, c) = (dir("x/aca"), dir("y/aca"), dir("z/home"));
    // Register a directory as a group: the name is the directory name, and doing it again gives the same one
    assert_eq!(sb.aca(&["group", "add", &a]), "aca");
    assert_eq!(sb.aca(&["group", "add", &a]), "aca");
    // The same name already has another directory: number onward. new -g . goes into the group decided by directory, with the
    // directory recorded as its default directory
    let id = sb.aca(&["new", "fake", &b, "--hold", "-g", "."]);
    assert_eq!(sb.json(&["show", &id, "--json"])["group"], "aca-2");
    assert_eq!(sb.aca(&["group", "dir", "aca-2"]), b);

    // Rename (under another parent): the sessions move along, and what the UI remembers about it (workspace, the group being
    // viewed) changes with it. A view file from when groups could fold still reads; that line isn't written back
    let ui = sb.home.join("state/ui");
    write(&ui.join("spaces"), "group\tdefault\taca-2\th\ngroup\tdefault\taca\t2\n");
    write(&ui.join("view"), "group=aca-2\nfolded=aca-2\tother\n");
    assert_eq!(sb.aca(&["group", "rename", "aca-2", "lab/aca"]), "aca-2 → lab/aca,1 个会话一并移动");
    assert_eq!(sb.json(&["show", &id, "--json"])["group"], "lab/aca");
    let spaces = std::fs::read_to_string(ui.join("spaces")).unwrap();
    assert!(spaces.contains("group\tdefault\tlab/aca\th\n") && spaces.contains("group\tdefault\taca\t2\n"), "{spaces}");
    let view = std::fs::read_to_string(ui.join("view")).unwrap();
    assert!(view.contains("group=lab/aca\n") && !view.contains("folded="), "{view}");
    // Clashes with an existing one: not changed
    let o = sb.run(&["group", "rename", "lab/aca", "ACA"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("aca 已存在"));

    // Default directory: change it; one belonging to another group isn't accepted; '' = none
    sb.aca(&["group", "dir", "lab/aca", &c]);
    assert_eq!(sb.aca(&["group", "dir", "lab/aca"]), c);
    let o = sb.run(&["group", "dir", "lab/aca", &a]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("分组 aca"));
    sb.aca(&["group", "dir", "lab/aca", ""]);
    assert_eq!(sb.aca(&["group", "dir", "lab/aca"]), "");
    // move .: b is no longer anyone's default directory, so it's decided by name again (aca has another directory → aca-2), and
    // it says which one it went into
    assert_eq!(sb.aca(&["move", &id, "."]), "aca-2");
    assert_eq!(sb.aca(&["group", "dir", "aca-2"]), b);
    // The parent directory's name is an existing group: nest under it (the same rules as N in the UI), with the directory recorded
    // as the new group's default directory
    sb.aca(&["group", "create", "work"]);
    let d = dir("w/work/new thing");
    let id = sb.aca(&["new", "fake", &d, "--hold", "-g", "."]);
    assert_eq!(sb.json(&["show", &id, "--json"])["group"], "work/new-thing");
    assert_eq!(sb.aca(&["group", "dir", "work/new-thing"]), d);
}

#[test]
fn the_mod_rides_along_only_on_a_claude_build_that_takes_it_and_keeps_reporting() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let Some(sb) = Sandbox::new("mod") else { return };
    let cfg = sb.home.join("config/config.toml");
    write(&cfg, &(std::fs::read_to_string(&cfg).unwrap() + "[tools.claude]\nmod = true\n"));
    // A fake claude: on `plugin validate` it prints report.json (the real one has this build check the mod itself), otherwise it's
    // a fake agent that records the arguments it got.
    // Every "installed" build changes the file (one more #), so the verdict is made again per build
    let bin = sb.home.join("bin");
    let install = |build: usize, report: &str| {
        write(&bin.join("report.json"), report);
        let script = format!(
            "#!/bin/sh\n{}\nhere=\"$(dirname \"$0\")\"\nif [ \"$1\" = plugin ]; then cat \"$here/report.json\"; exit 0; fi\n\
             echo \"$*\" > \"$here/argv.$ACA_SESSION_ID\"\nexec sleep 600\n",
            "#".repeat(build)
        );
        write(&bin.join("claude"), &script);
        std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let start = || {
        let o = sb.cmd().env("PATH", &path).args(["new", "claude", &sb.proj()]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
        let argv = bin.join(format!("argv.{id}"));
        for _ in 0..50 {
            if argv.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        (id, std::fs::read_to_string(argv).unwrap())
    };
    let hands_over = |id: &str, measure: &str| {
        let mut c = sb.cmd();
        c.args(["limits", "--mod"]).env("ACA_SESSION_ID", id).env("CLAUDE_CONFIG_DIR", sb.home.join("acct"));
        let mut child = c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(measure.as_bytes()).unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success() && o.stdout.is_empty());
    };
    let state = sb.home.join("state");
    let whys = || -> String {
        let all = std::fs::read_dir(state.join("mod/builds")).unwrap().flatten();
        all.map(|e| {
            serde_json::from_str::<Value>(&std::fs::read_to_string(e.path()).unwrap()).unwrap()["why"].as_str().unwrap().to_string()
        })
        .collect::<Vec<_>>()
        .join("|")
    };
    let passes = r#"{"success":true,"manifest":{"errors":[]},"contents":[{"errors":[],"notes":["./register.js hooks: session.measure","./register.js calls: $.process.run"]}]}"#;
    let mods = state.join("hooks/claude-mod/aca");
    let carried = format!("--plugin-dir {}", mods.display());

    // This build passes the check: the mod is passed at start, like hooks, with the files in aca's own state directory
    install(1, passes);
    let (one, argv) = start();
    assert!(argv.contains(&carried) && argv.contains("--settings"), "{argv}");
    assert!(mods.join("hooks/register.js").is_file() && mods.join(".claude-plugin/plugin.json").is_file());
    // It hands in usage once: recorded in this session's own file, with the account being the config directory in its environment
    let measure = r#"{"context":{"window":1000000},"rateLimits":[{"kind":"five_hour","percentUsed":6,"resetsAt":"2026-10-09T11:40:00.000Z"}],"cost":{"usd":0},"changed":["rateLimits"]}"#;
    hands_over(&one, measure);
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(state.join(format!("ui/limits/mod-{one}.json"))).unwrap()).unwrap();
    assert_eq!(rec["dir"].as_str().unwrap(), sb.home.join("acct").to_string_lossy());
    assert_eq!((rec["wins"][0]["label"].as_str(), rec["wins"][0]["used"].as_f64()), (Some("5h"), Some(6.0)));
    assert!(state.join(format!("mod/heard/{one}")).is_file());
    // What it hands in can't be recognized (the event's shape changed): not passed after this build
    hands_over(&one, r#"{"context":{"window":1000000},"limits":[]}"#);
    let (two, argv) = start();
    assert!(!argv.contains("--plugin-dir"), "{argv}");
    assert!(whys().contains("无法识别") && !state.join(format!("mod/up/{two}")).exists());

    // Another build, and it says itself that it doesn't know this event: not passed
    install(
        2,
        r#"{"success":false,"manifest":{"errors":[]},"contents":[{"errors":[{"path":"modules../register.js","message":"aca: /x/register.js:5: \"session.measure\" is not an event; $ is always spelled $.noun.event(...)"}],"notes":[]}]}"#,
    );
    assert!(!start().1.contains("--plugin-dir"));
    assert!(whys().contains("is not an event"));

    // Yet another build: passes the check and is passed, but hands in nothing during a whole turn (the call on `$` was renamed, which
    // the check can't catch): not passed on the next start
    install(3, passes);
    let (four, argv) = start();
    assert!(argv.contains(&carried), "{argv}");
    // It has been up a minute (turn the recorded time back), and a turn finishes
    let up = state.join(format!("mod/up/{four}"));
    let (key, at) =
        std::fs::read_to_string(&up).unwrap().trim().split_once(' ').map(|(k, t)| (k.to_string(), t.parse::<u64>().unwrap())).unwrap();
    write(&up, &format!("{key} {}\n", at - 60));
    let pane = sb.json(&["show", &four, "--json"])["pane"].as_str().unwrap().to_string();
    sb.hook(&four, &pane, r#"{"hook_event_name":"UserPromptSubmit","session_id":"c4","prompt":"hi"}"#);
    sb.hook(&four, &pane, r#"{"hook_event_name":"Stop","session_id":"c4"}"#);
    assert!(!start().1.contains("--plugin-dir"));
    assert!(whys().contains("未收到任何用量"), "{}", whys());
}

#[test]
fn a_finished_worker_rings_its_parents_doorbell_and_claude_takes_the_notice_itself() {
    let Some(sb) = Sandbox::new("bell") else { return };
    let lead = sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    // Nobody asked to be notified: finishing wakes no one
    let quiet = sb.aca(&["new", "fake", &sb.proj(), "-t", "安静", "--parent", &lead]);
    sb.hook(&quiet, &sb.pane(&quiet), r#"{"hook_event_name":"Stop","session_id":"c9","last_assistant_message":"好"}"#);
    assert_eq!(sb.json(&["inbox", &lead, "--json"]), serde_json::json!([]));
    let w = sb.aca(&["new", "fake", &sb.proj(), "-t", "读文档", "--parent", &lead, "--notify", "all"]);
    // The lead's Claude has started: the extra command run on SessionStart answers with a file for it to watch (its doorbell), and
    // stdout holds only that
    let o = sb.as_session(&lead, &["inbox", "watch"], r#"{"hook_event_name":"SessionStart","source":"startup"}"#);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let bell = v["hookSpecificOutput"]["watchPaths"][0].as_str().unwrap().to_string();
    assert!(bell.ends_with(&format!("inbox/{lead}.bell")), "{v}");
    // Its working directory changed: Claude swaps its watch list for what CwdChanged returns, so the same file is reported
    // again, under that event's name
    let o = sb.as_session(&lead, &["inbox", "watch"], r#"{"hook_event_name":"CwdChanged","old_cwd":"/a","new_cwd":"/tmp"}"#);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "CwdChanged");
    assert_eq!(v["hookSpecificOutput"]["watchPaths"][0].as_str(), Some(bell.as_str()));
    let before = std::fs::read_to_string(&bell).unwrap();

    // The worker finished a turn: its own hook puts a notice in the lead's inbox, then rings the doorbell
    sb.hook(&w, &sb.pane(&w), r#"{"hook_event_name":"Stop","session_id":"c1","last_assistant_message":"整理好了,\n结论写在 notes.md"}"#);
    assert!(sb.until(|| std::fs::read_to_string(&bell).unwrap() != before), "门铃没响");
    let inbox = sb.json(&["inbox", &lead, "--json"]);
    assert_eq!(
        (inbox[0]["kind"].as_str(), inbox[0]["what"].as_str(), inbox[0]["from"].as_str(), inbox[0]["stage"].as_str()),
        (Some("notice"), Some("turn.done"), Some(w.as_str()), Some("pending"))
    );

    // Claude sees the doorbell change and runs the fetching hook in the background: written to stderr with exit code 2, it gives
    // that text to the model as a system reminder
    let take = || {
        sb.as_session(&lead, &["inbox", "take"], &format!(r#"{{"hook_event_name":"FileChanged","file_path":"{bell}","event":"change"}}"#))
    };
    let o = take();
    let said = String::from_utf8_lossy(&o.stderr).to_string();
    assert_eq!(o.status.code(), Some(2), "{said}");
    assert!(
        said.starts_with("1 条新消息(")
            && said.contains(&format!("{w}「读文档」fake · 跑完一轮"))
            && said.contains("最后一句:「整理好了, 结论写在 notes.md」")
            && said.contains(&format!("全文:aca output {w}")),
        "{said}"
    );
    assert!(o.stdout.is_empty());
    // Already fetched: ringing again does nothing
    assert_eq!(take().status.code(), Some(0));
    assert_eq!(sb.json(&["inbox", &lead, "--json"])[0]["stage"], "got");

    // A session you started yourself (no parent) finished: nobody is notified
    sb.hook(&lead, &sb.pane(&lead), r#"{"hook_event_name":"Stop","session_id":"c0","last_assistant_message":"好"}"#);
    let names = sb.event_names();
    assert_eq!(names.iter().filter(|n| *n == "message.sent").count(), 1, "{names:?}");
    assert_eq!(names.iter().filter(|n| *n == "message.delivered").count(), 1, "{names:?}");
    // Deleting the session removes its inbox too
    sb.aca(&["rm", &lead]);
    assert!(!Path::new(&bell).exists());
}

/// A fork is another path you continue yourself: a peer of the original session (no parent; who it forked from is in fork_of),
/// listed right after the original session's family;
/// when it finishes it doesn't notify the original session, and the original session can't answer for it
#[test]
fn a_fork_is_a_peer_listed_right_after_the_original_and_not_one_it_dispatched() {
    let Some(sb) = Sandbox::new("fork-peer") else { return };
    // The config has workers notify whoever sent them out whenever they finish: a fork shouldn't notify too
    let cfg = sb.home.join("config/config.toml");
    write(&cfg, &format!("{}[inbox]\nnotify = [\"done\"]\n", std::fs::read_to_string(&cfg).unwrap()));
    let lead = sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    let worker = sb.aca(&["new", "fake", &sb.proj(), "-t", "读文档", "--parent", &lead]);
    let other = sb.aca(&["new", "fake", &sb.proj(), "-t", "别的事"]);
    let fork = sb.aca(&["fork", &lead]);
    // A fork of the worker: also top-level, listed after the one that sent out the worker
    let wfork = sb.aca(&["fork", &worker, "-t", "换个读法"]);

    let rows = sb.json(&["ls", "--json", "--no-screen"]);
    let row = |id: &str| rows.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap_or_else(|| panic!("{id}:{rows}")).clone();
    let of = |id: &str| (row(id)["parent"].as_str().map(String::from), row(id)["fork_of"].as_str().map(String::from));
    assert_eq!(of(&fork), (Some(String::new()), Some(lead.clone())));
    assert_eq!(of(&wfork), (Some(String::new()), Some(worker.clone())));
    assert_eq!(of(&worker), (Some(lead.clone()), Some(String::new())));
    assert_eq!((row(&fork)["title"].as_str(), row(&wfork)["title"].as_str()), (Some("主线 (分叉)"), Some("换个读法")));
    let mut by_order: Vec<(i64, String)> =
        rows.as_array().unwrap().iter().map(|r| (r["order"].as_i64().unwrap(), r["id"].as_str().unwrap().to_string())).collect();
    by_order.sort();
    let ids: Vec<String> = by_order.into_iter().map(|x| x.1).collect();
    assert_eq!(ids, [lead.clone(), wfork.clone(), fork.clone(), worker.clone(), other]);

    // In the events: who it forked from, no parent
    let made: Vec<Value> = sb.aca(&["events", "-n", "100"]).lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
    let made = made.iter().find(|e| e["event"] == "session.created" && e["session"] == fork.as_str()).unwrap();
    assert_eq!((made["fork"].as_bool(), made["fork_of"].as_str(), made["parent"].as_str()), (Some(true), Some(lead.as_str()), Some("")));

    // The fork finishes a turn: it isn't in the lead's inbox; the worker finishing still notifies
    sb.hook(&fork, &sb.pane(&fork), r#"{"hook_event_name":"Stop","session_id":"f1","last_assistant_message":"好"}"#);
    assert_eq!(sb.json(&["inbox", &lead, "--json"]), serde_json::json!([]));
    sb.hook(&worker, &sb.pane(&worker), r#"{"hook_event_name":"Stop","session_id":"w1","last_assistant_message":"读完了"}"#);
    assert_eq!(sb.json(&["inbox", &lead, "--json"])[0]["from"], worker.as_str());
    // Sending to parent from the fork: it has none
    assert!(!sb.as_session(&fork, &["msg", "parent", "在吗"], "").status.success());
}

#[test]
fn a_message_to_codex_goes_into_its_own_queue_and_waits_while_it_is_busy() {
    let Some(sb) = Sandbox::new("queue") else { return };
    sb.screen_tools();
    let cx = sb.aca(&["new", "fcx", &sb.proj()]);
    let pane = sb.pane(&cx);
    let hook = |json: &str| sb.hook_as("codex", &cx, &pane, json);
    let stage = |n: usize| sb.json(&["inbox", &cx, "--json"])[n]["stage"].as_str().unwrap().to_string();

    // It hasn't talked yet: with no conversation id it can't be queued; there's no input box on screen either, so nothing is typed.
    // Hold it for now
    let said = sb.aca(&["msg", &cx, "先看看 README"]);
    assert!(said.contains("暂存"), "{said}");
    assert_eq!(sb.queued(), "");
    // It said something and this turn finished: its own Stop hook queues the held one
    hook(r#"{"hook_event_name":"SessionStart","session_id":"th-1","source":"startup"}"#);
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"第一句"}"#);
    hook(r#"{"hook_event_name":"Stop","session_id":"th-1","last_assistant_message":"好"}"#);
    assert!(sb.until(|| sb.queued().contains("先看看 README")), "没排进去:{}", sb.queued());
    // With a -c (writes within its own process and leaves, without bringing up the background server), the conversation id, the text
    let q = sb.queued();
    let args: Vec<&str> = q.lines().collect();
    assert_eq!(args[..6], ["-c", "notify=[]", "queue", "--thread", "th-1", "--message"]);
    // Sent by you: queued as is, without the "forwarded from another session" wrapper (Codex would take that as an injection)
    assert_eq!(args[6], "先看看 README", "{q}");
    assert!(sb.until(|| stage(0) == "queued"), "{}", stage(0));
    // Codex started a turn for it: UserPromptSubmit carries the original text, recorded as delivered
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"先看看 README"}"#);
    assert_eq!(stage(0), "got");

    // It's running: another message is held first, and queued as soon as this turn finishes
    sb.set_screen(&cx, &pane, "• Working (5s • esc to interrupt)");
    let said = sb.aca(&["msg", &cx, "再补一句"]);
    assert!(said.contains("它正在运行"), "{said}");
    sb.set_screen(&cx, &pane, "• 好了");
    hook(r#"{"hook_event_name":"Stop","session_id":"th-1","last_assistant_message":"好了"}"#);
    assert!(sb.until(|| sb.queued().contains("再补一句")), "{}", sb.queued());

    // You just interrupted it with Esc: don't push, wait for your next turn to finish
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"写一篇长文"}"#);
    hook(r#"{"hook_event_name":"Interrupt","session_id":"th-1"}"#);
    let said = sb.aca(&["msg", &cx, "第三句"]);
    assert!(said.contains("中断"), "{said}");
    assert!(!sb.queued().contains("第三句"));
}

#[test]
fn an_argument_that_is_not_utf8_is_reported_instead_of_a_panic() {
    use std::os::unix::ffi::OsStrExt;
    let Some(sb) = Sandbox::new("badarg") else { return };
    // What a real Codex once wrote: sh swallowed the first byte of a full-width comma into the variable name (`PID=$pid,`),
    // leaving half a character
    let o = sb.cmd().args(["msg", "x"]).arg(std::ffi::OsStr::from_bytes(b"PID=\xBC\x8C log")).output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("参数不是合法的 UTF-8") && !err.contains("panicked"), "{err}");
}

#[test]
fn a_scheduled_message_waits_until_it_is_due_and_one_can_go_to_yourself() {
    let Some(sb) = Sandbox::new("later") else { return };
    sb.screen_tools();
    let cx = sb.aca(&["new", "fcx", &sb.proj()]);
    let pane = sb.pane(&cx);
    let hook = |json: &str| sb.hook_as("codex", &cx, &pane, json);
    hook(r#"{"hook_event_name":"SessionStart","session_id":"th-1","source":"startup"}"#);
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"第一句"}"#);
    hook(r#"{"hook_event_name":"Stop","session_id":"th-1","last_assistant_message":"好"}"#);
    let inbox = || sb.json(&["inbox", &cx, "--json"]);

    // Scheduled: put in but not delivered yet, it's 「定时」 (scheduled); delivering another message doesn't bring it along
    let said = sb.aca(&["msg", "--in", "2s", &cx, "看一下 CI"]);
    assert!(said.contains("送达"), "{said}");
    assert_eq!(inbox()[0]["stage"], "scheduled");
    assert!(inbox()[0]["at"].as_u64().unwrap() > 0);
    assert!(sb.aca(&["inbox", &cx]).contains("[定时 "), "{}", sb.aca(&["inbox", &cx]));
    sb.aca(&["msg", &cx, "先看 README"]);
    assert!(sb.until(|| sb.queued().contains("先看 README")));
    assert!(!sb.queued().contains("看一下 CI"), "{}", sb.queued());
    // Due: goes into its queue as usual
    assert!(sb.until(|| sb.queued().contains("看一下 CI")), "到点没送:{}", sb.queued());
    assert!(sb.until(|| inbox()[0]["stage"] == "queued"), "{}", inbox()[0]);

    // One that isn't scheduled can't be sent to yourself; a scheduled one can, waking it when due and saying it scheduled it itself
    let err = |o: &Output| String::from_utf8_lossy(&o.stderr).to_string();
    assert!(err(&sb.as_session(&cx, &["msg", &cx, "x"], "")).contains("不能发送给自己"));
    let o = sb.as_session(&cx, &["msg", "--in", "1s", &cx, "回来看看测试"], "");
    assert!(o.status.success(), "{}", err(&o));
    assert!(sb.until(|| sb.queued().contains("回来看看测试")), "{}", sb.queued());
    let q = sb.queued();
    let last = q.split("----\n").filter(|b| b.contains("回来看看测试")).last().unwrap();
    assert!(last.contains("[aca] 定时到点了(你 刚刚定的):\n回来看看测试"), "{last}");

    // A duration written wrong, or too long
    assert!(err(&sb.run(&["msg", "--in", "soon", &cx, "x"])).contains("--in"));
    assert!(!sb.run(&["msg", "--in", "8d", &cx, "x"]).status.success());
}

#[test]
fn without_a_doorbell_a_message_is_typed_in_only_when_the_input_box_is_empty() {
    let Some(sb) = Sandbox::new("type") else { return };
    sb.screen_tools();
    // Started the Claude way, but not hooked to the doorbell this time (like one adopted and not restarted yet): typing is the only way
    let id = sb.aca(&["new", "scr", &sb.proj()]);
    let pane = sb.pane(&id);
    // The input box has something you haven't sent: don't type, look again in a while
    sb.set_screen(&id, &pane, "❯ 人打的草稿");
    let said = sb.aca(&["msg", &id, "补一下测试"]);
    assert!(said.contains("输入框中有你未发送的内容"), "{said}");
    // Only a gray placeholder hint: type it in, together with the held one
    sb.set_screen(&id, &pane, "❯ \x1b[2mTry \"fix typecheck errors\"\x1b[0m");
    let said = sb.aca(&["msg", &id, "跑一下 e2e"]);
    assert!(said.contains("已输入并发送"), "{said}");
    let screen = sb.wait_screen(&pane, "跑一下 e2e");
    assert!(screen.contains("补一下测试") && screen.contains("跑一下 e2e") && !screen.contains("[aca]"), "{screen}");
    let inbox = sb.json(&["inbox", &id, "--json"]);
    assert!(inbox.as_array().unwrap().iter().all(|m| m["stage"] == "got"), "{inbox}");
    // A shell session doesn't take it: typed in, it would become a command
    let sh = sb.aca(&["new", "shell", &sb.proj()]);
    assert!(sb.aca(&["msg", &sh, "ls"]).contains("shell"));
    // No such session, no message given
    assert!(!sb.run(&["msg", "nope", "x"]).status.success());
    assert!(!sb.run(&["msg", &id, "  "]).status.success());
}

#[test]
fn a_waiting_worker_is_answered_by_number_and_only_by_the_one_that_sent_it_out() {
    let Some(sb) = Sandbox::new("answer") else { return };
    sb.screen_tools();
    let lead = sb.aca(&["new", "fake", &sb.proj()]);
    let other = sb.aca(&["new", "fake", &sb.proj()]);
    let w = sb.aca(&["new", "scr", &sb.proj(), "--parent", &lead, "--notify", "waiting"]);
    let pane = sb.pane(&w);
    // It asked a multiple-choice question (as in 2.1.294)
    sb.set_screen(&w, &pane, " ☐ 测试选择\n测试选 A 还是 B？\n❯ 1. A\n  2. B\nEnter to select · ↑/↓ to navigate · Esc to cancel");
    sb.hook(
        &w,
        &pane,
        r#"{"hook_event_name":"PreToolUse","session_id":"c1","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"测试选 A 还是 B？","multiSelect":false,"options":[{"label":"A"},{"label":"B"}]}]}}"#,
    );
    // The one that sent it out gets a notice: what was asked, which options there are, how to answer
    let notice = sb.json(&["inbox", &lead, "--json"])[0]["text"].as_str().unwrap().to_string();
    assert_eq!(notice, format!("在问:「测试选 A 还是 B？」\n选项:1 A  2 B  回答:aca answer {w} <选项号>"));
    let shown = sb.aca(&["answer", &w]);
    assert!(shown.contains("  1 A\n  2 B") && shown.contains("aca answer"), "{shown}");

    // A plain send doesn't type into a confirmation box
    let o = sb.run(&["send", &w, "-m", "hi"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("aca answer"));
    // Other sessions can't answer for it; the option number has to match
    let o = sb.as_session(&other, &["answer", &w, "2"], "");
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("不是你派出的会话"));
    assert!(!sb.as_session(&lead, &["answer", &w, "3"], "").status.success());
    // The one that sent it out can: presses 2 (the fake agent doesn't read keys, the terminal echoes it on screen)
    let o = sb.as_session(&lead, &["answer", &w, "2"], "");
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("2 B"), "{}", String::from_utf8_lossy(&o.stderr));
    let screen = sb.wait_screen(&pane, "cancel\n2");
    assert!(screen.ends_with("Esc to cancel\n2"), "{screen}");
    let answered = sb.aca(&["events", "-n", "100"]).lines().find(|l| l.contains("ask.answered")).unwrap().to_string();
    assert!(answered.contains(&format!("\"by\":\"{lead}\"")) && answered.contains("\"answer\":\"2 B\""), "{answered}");
    // Answered: the question is cleared
    assert_eq!(sb.json(&["show", &w, "--json"])["run"]["ask"], "");
}

/// The gate at start (Codex's trust box) exists only on screen: numbered options are listed, and answering moves there with
/// arrow keys, then presses Enter
#[test]
fn a_startup_gate_seen_only_on_screen_is_answered_by_walking_to_the_option() {
    let Some(sb) = Sandbox::new("gate") else { return };
    sb.screen_tools();
    let lead = sb.aca(&["new", "fake", &sb.proj()]);
    let w = sb.aca(&["new", "fcx", &sb.proj(), "--parent", &lead]);
    let pane = sb.pane(&w);
    sb.set_screen(
        &w,
        &pane,
        "Folder access\n  /p/api\n  Trust this folder? Codex can read, edit, and run files here.\n› 1. Trust and continue\n  2. Quit\n  enter continue · esc quit",
    );
    let shown = sb.aca(&["answer", &w]);
    assert!(
        shown.contains("正在询问:Folder access · /p/api · Trust this folder?") && shown.contains("  1 Trust and continue\n  2 Quit"),
        "{shown}"
    );
    assert!(!sb.as_session(&lead, &["answer", &w, "3"], "").status.success());
    // Pick 2: one down, then Enter (the fake agent doesn't read keys, the terminal echoes the arrow key as ^[[B)
    let o = sb.as_session(&lead, &["answer", &w, "2"], "");
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("2 Quit"), "{}", String::from_utf8_lossy(&o.stderr));
    let screen = sb.wait_screen(&pane, "^[[B");
    assert!(screen.contains("esc quit\n^[[B"), "{screen}");
    let answered = sb.aca(&["events", "-n", "100"]).lines().find(|l| l.contains("ask.answered")).unwrap().to_string();
    assert!(answered.contains("\"kind\":\"gate\"") && answered.contains("\"answer\":\"2 Quit\""), "{answered}");
}

#[test]
fn waiting_for_a_worker_returns_when_its_turn_ends_and_its_notice_does_not_wake_again() {
    let Some(sb) = Sandbox::new("wait") else { return };
    sb.screen_tools();
    let lead = sb.aca(&["new", "fake", &sb.proj()]);
    sb.as_session(&lead, &["inbox", "watch"], "{}");
    let w = sb.aca(&["new", "scr", &sb.proj(), "--parent", &lead, "--notify", "done"]);
    let pane = sb.pane(&w);
    sb.set_screen(&w, &pane, "✻ Pondering… (3s · ↓ 1k tokens)");
    sb.hook(&w, &pane, r#"{"hook_event_name":"UserPromptSubmit","session_id":"c1","prompt":"读文档"}"#);

    // The lead hangs on, waiting for it
    let mut child =
        sb.cmd().args(["wait", &w, "--timeout", "20"]).env("ACA_SESSION_ID", &lead).stdout(std::process::Stdio::piped()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(child.try_wait().unwrap().is_none(), "它还在跑,不该等到");
    sb.set_screen(&w, &pane, "❯ ");
    sb.hook(&w, &pane, r#"{"hook_event_name":"Stop","session_id":"c1","last_assistant_message":"读完了"}"#);
    let o = child.wait_with_output().unwrap();
    assert_eq!((o.status.code(), String::from_utf8_lossy(&o.stdout).trim()), (Some(0), format!("{w} 完成一轮:「读完了」").as_str()));
    // Its notice is already recorded as delivered: the doorbell ringing won't wake anyone for it again
    assert_eq!(sb.json(&["inbox", &lead, "--json"])[0]["stage"], "got");
    assert_eq!(sb.as_session(&lead, &["inbox", "take"], "{}").status.code(), Some(0));

    // One that never starts working: returns when time is up, exit code 124
    let idle = sb.aca(&["new", "scr", &sb.proj()]);
    let o = sb.run(&["wait", &idle, "--timeout", "1"]);
    assert_eq!((o.status.code(), String::from_utf8_lossy(&o.stdout).trim()), (Some(124), format!("{idle} 尚未完成(已超时)").as_str()));
}

#[test]
fn a_turn_ended_with_background_tasks_still_running_is_not_what_wait_returns_on() {
    let Some(sb) = Sandbox::new("waitbg") else { return };
    sb.screen_tools();
    let w = sb.aca(&["new", "scr", &sb.proj()]);
    let pane = sb.pane(&w);
    sb.set_screen(&w, &pane, "✻ Pondering… (3s · ↓ 1k tokens)");
    sb.hook(&w, &pane, r#"{"hook_event_name":"UserPromptSubmit","session_id":"c1","prompt":"录屏"}"#);
    let mut child = sb.cmd().args(["wait", &w, "--timeout", "20"]).stdout(std::process::Stdio::piped()).spawn().unwrap();

    // Claude ends the turn while its own background task runs: it'll be woken when the task ends, so keep waiting
    sb.set_screen(&w, &pane, "❯ ");
    sb.hook(&w, &pane, r#"{"hook_event_name":"Stop","session_id":"c1","last_assistant_message":"等录完","background_tasks":[{"id":"b1"}]}"#);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(child.try_wait().unwrap().is_none(), "后台任务还在跑,不该等到");

    // The task ends and wakes it for a new turn; this one ends with nothing left in the background
    sb.set_screen(&w, &pane, "✻ Pondering… (1s · ↓ 1k tokens)");
    sb.hook(&w, &pane, r#"{"hook_event_name":"UserPromptSubmit","session_id":"c1","prompt":"<task-notification>"}"#);
    sb.set_screen(&w, &pane, "❯ ");
    sb.hook(&w, &pane, r#"{"hook_event_name":"Stop","session_id":"c1","last_assistant_message":"录好了","background_tasks":[]}"#);
    let o = child.wait_with_output().unwrap();
    assert_eq!((o.status.code(), String::from_utf8_lossy(&o.stdout).trim()), (Some(0), format!("{w} 完成一轮:「录好了」").as_str()));
}

#[test]
fn a_plugin_installed_from_git_brings_its_claude_plugin_while_enabled_and_goes_away_cleanly() {
    let Some(sb) = Sandbox::new("plugins") else { return };
    // A plugin repo: the manifest says to pass Claude the plugin directory claude/
    let src = sb.home.join("src");
    std::fs::create_dir_all(src.join("claude/.claude-plugin")).unwrap();
    std::fs::create_dir_all(src.join("claude/commands")).unwrap();
    let manifest = |v: &str| format!("id = \"t.deleg\"\nname = \"Deleg\"\nversion = \"{v}\"\n[agents.claude]\nplugin_dir = \"claude\"\n");
    std::fs::write(src.join("aca-plugin.toml"), manifest("0.1.0")).unwrap();
    std::fs::write(src.join("claude/.claude-plugin/plugin.json"), r#"{"name":"deleg"}"#).unwrap();
    std::fs::write(src.join("claude/commands/hello.md"), "---\ndisable-model-invocation: true\n---\nhi\n").unwrap();
    git(&src, &["init", "-q"]);
    git(&src, &["add", "."]);
    git(&src, &["commit", "-q", "-m", "v1"]);
    let v1 = git(&src, &["rev-parse", "HEAD"]);
    // A fake agent started the Claude way that prints its arguments on screen
    let cfg = sb.home.join("config/config.toml");
    std::fs::write(sb.home.join("args.sh"), "#!/bin/sh\necho \"args:[$*]\"\nexec sleep 600\n").unwrap();
    let text = std::fs::read_to_string(&cfg).unwrap()
        + &format!("[tools.argc]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", sb.home.join("args.sh").display());
    std::fs::write(&cfg, text).unwrap();
    // The pane is 80 columns, so long arguments wrap: join them back before looking
    let args_of = |id: &str| sb.wait_screen(&sb.pane(id), "]").replace('\n', "");

    // Install from git: cloned into aca's data directory, where it came from is recorded, passed to Claude
    let url = format!("file://{}", src.display());
    assert!(sb.aca(&["plugin", "install", &url]).contains("已安装 t.deleg(Deleg 0.1.0)"));
    let dir = sb.home.join("data/plugins/t.deleg");
    assert!(dir.join("claude/commands/hello.md").is_file());
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("● t.deleg  Deleg 0.1.0") && ls.contains(&format!("安装自 {url}")) && ls.contains("提供给 claude"), "{ls}");
    // Enabled: when Claude starts, its plugin directory is passed (for that one start only)
    let a = sb.aca(&["new", "argc", &sb.proj()]);
    let want = format!("--plugin-dir {}", dir.join("claude").canonicalize().unwrap().display());
    assert!(args_of(&a).contains(&want), "{}", args_of(&a));
    // Disabled: new starts don't get it
    sb.aca(&["plugin", "disable", "t.deleg"]);
    let b = sb.aca(&["new", "argc", &sb.proj()]);
    assert!(!args_of(&b).contains("--plugin-dir"), "{}", args_of(&b));
    sb.aca(&["plugin", "enable", "t.deleg"]);
    // Installing the same one again: no overwrite
    assert!(!sb.run(&["plugin", "install", &url]).status.success());
    // A new version in the repo: update pulls it and rereads the manifest
    std::fs::write(src.join("aca-plugin.toml"), manifest("0.2.0")).unwrap();
    git(&src, &["commit", "-q", "-am", "v2"]);
    let said = sb.aca(&["plugin", "update"]);
    assert!(said.contains("t.deleg:") && said.contains("→") && said.contains("Deleg 0.2.0"), "{said}");
    assert!(sb.aca(&["plugin", "update", "t.deleg"]).contains("已是最新版本"));
    // Remove: deleted along with its directory, the registration gone; the plugin's own config stays (the directory aca made for it
    // doesn't stay if it's empty, see the aca.copy test)
    write(&sb.home.join("config/plugins/t.deleg/mine.toml"), "x = 1\n");
    assert!(sb.aca(&["plugin", "remove", "t.deleg"]).contains("已删除 t.deleg"));
    // No plugin is listed anymore; the session that started with it is named: it needs a restart to drop it
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(!dir.exists() && !ls.lines().any(|l| l.starts_with(['●', '○'])) && ls.contains("重启"), "{ls}");
    assert!(sb.home.join("config/plugins/t.deleg/mine.toml").is_file());
    // Pinned to a version: update leaves it alone
    sb.aca(&["plugin", "install", &url, "--ref", &v1]);
    assert!(sb.aca(&["plugin", "update", "t.deleg"]).contains("固定在"));
    assert!(sb.aca(&["plugin", "ls"]).contains("Deleg 0.1.0"));
    sb.aca(&["plugin", "remove", "t.deleg"]);
    // A local directory: just a link; removing leaves the directory alone
    assert!(sb.aca(&["plugin", "install", &src.to_string_lossy()]).contains("已安装 t.deleg"));
    assert!(sb.aca(&["plugin", "remove", "t.deleg"]).contains("未删除"));
    assert!(src.join("aca-plugin.toml").is_file());
}

#[test]
fn a_plugin_whose_auto_setting_is_on_gives_claude_its_commands_without_the_only_when_typed_line() {
    let Some(sb) = Sandbox::new("auto") else { return };
    // The manifest: the auto setting decides whether the agent may use the commands in claude/ by itself; the command file says
    // "only enters the context when you type it"
    let dir = sb.home.join("autop");
    write(
        &dir.join("aca-plugin.toml"),
        "id = \"t.auto\"\nname = \"Auto\"\nversion = \"1\"\n[settings.auto]\ndefault = true\n[agent]\nfor = [\"claude\"]\ndir = \"claude\"\nauto = \"auto\"\n",
    );
    write(&dir.join("claude/.claude-plugin/plugin.json"), r#"{"name":"auto"}"#);
    let cmd = "---\ndescription: go\ndisable-model-invocation: true\n---\ngo $ARGUMENTS\n";
    write(&dir.join("claude/commands/go.md"), cmd);
    sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    // A fake agent started the Claude way that writes its arguments into a file, one per line
    write(
        &sb.home.join("args.sh"),
        &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}/args-'\"$ACA_SESSION_ID\"\nexec sleep 600\n", sb.home.display()),
    );
    let cfg = sb.home.join("config/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap()
        + &format!("[tools.argc]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", sb.home.join("args.sh").display());
    std::fs::write(&cfg, text).unwrap();
    let plugin_dirs = |id: &str| -> Vec<PathBuf> {
        let f = sb.home.join(format!("args-{id}"));
        for _ in 0..50 {
            if let Ok(t) = std::fs::read_to_string(&f) {
                let args: Vec<&str> = t.lines().collect();
                return args.windows(2).filter(|w| w[0] == "--plugin-dir").map(|w| Path::new(w[1]).canonicalize().unwrap()).collect();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("{id} 没起来");
    };

    // On (the default): Claude gets a copy in aca's data directory, with that line gone; the files in the plugin directory are untouched
    let a = sb.aca(&["new", "argc", &sb.proj(), "-t", "开着起的"]);
    let copy = sb.home.join("data/claude-plugins/t-auto/local");
    assert_eq!(plugin_dirs(&a), [copy.canonicalize().unwrap()]);
    assert_eq!(std::fs::read_to_string(copy.join("commands/go.md")).unwrap(), "---\ndescription: go\n---\ngo $ARGUMENTS\n");
    assert!(copy.join(".claude-plugin/plugin.json").is_file());
    assert_eq!(std::fs::read_to_string(dir.join("claude/commands/go.md")).unwrap(), cmd);
    assert!(!sb.aca(&["plugin", "ls"]).contains("重启"));
    // Off: new starts take the plugin directory directly; the one started while on changes only after a restart
    sb.aca(&["plugin", "setting", "t.auto", "auto", "off"]);
    let b = sb.aca(&["new", "argc", &sb.proj(), "-t", "关着起的"]);
    assert_eq!(plugin_dirs(&b), [dir.join("claude").canonicalize().unwrap()]);
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("「开着起的」") && !ls.contains("「关着起的」") && ls.contains("重启"), "{ls}");
    // Back to the default (on): the other way around
    sb.aca(&["plugin", "setting", "t.auto", "auto", "--reset"]);
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(!ls.contains("「开着起的」") && ls.contains("「关着起的」"), "{ls}");
}

#[test]
fn the_delegate_plugin_says_continue_to_a_worker_whose_turn_failed_for_a_transient_reason() {
    let Some(sb) = Sandbox::new("delegate") else { return };
    sb.screen_tools();
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/delegate").canonicalize().unwrap();
    assert!(sb.aca(&["plugin", "link", &plugin.to_string_lossy()]).contains("已链接 aca.delegate"));
    // It passes Claude and Codex the same plugin directory, which holds /delegate. The file says the model doesn't call it by
    // itself, which doesn't hold while the auto setting (on by default) is on
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("● aca.delegate") && ls.contains("提供给 claude、codex"), "{ls}");
    let cmd = std::fs::read_to_string(plugin.join("claude/commands/delegate.md")).unwrap();
    assert!(cmd.starts_with("---\n") && cmd.contains("\ndisable-model-invocation: true\n"));
    assert_eq!(sb.aca(&["plugin", "setting", "aca.delegate", "auto"]), "true");
    // Settings: the default value, a change recorded in plugins.toml and read back; what it says for the worker changed to 「继续」
    // ("continue" in Chinese)
    assert_eq!(sb.aca(&["plugin", "setting", "aca.delegate", "worker"]), "codex");
    sb.aca(&["plugin", "setting", "aca.delegate", "say", "继续"]);
    assert_eq!(sb.aca(&["plugin", "setting", "aca.delegate", "say"]), "继续");
    assert!(sb.aca(&["plugin", "setting", "aca.delegate"]).contains("say = 继续(已修改)"));
    assert!(!sb.run(&["plugin", "setting", "aca.delegate", "nope", "1"]).status.success());
    // Read by the type of the default value: an integer one won't take text; a text one given true is still text
    let o = sb.run(&["plugin", "setting", "aca.delegate", "max_continue", "many"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("应为整数"));
    sb.aca(&["plugin", "setting", "aca.delegate", "say", "true"]);
    assert!(std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("say = \"true\""));
    sb.aca(&["plugin", "setting", "aca.delegate", "say", "继续"]);

    let lead = sb.aca(&["new", "fake", &sb.proj()]);
    let w = sb.aca(&["new", "scr", &sb.proj(), "--parent", &lead]);
    let mine = sb.aca(&["new", "scr", &sb.proj(), "--parent", "-"]);
    // Hooks are started by the hook process and inherit its environment: the test doesn't wait 30 seconds
    let fail = |id: &str, error: &str| {
        let pane = sb.pane(id);
        sb.set_screen(id, &pane, "❯ ");
        // Wait for the fake agent to draw this screen: otherwise its clear-and-redraw would wipe out the 「继续」 typed in for it too
        sb.wait_screen(&pane, "❯");
        let payload = serde_json::json!({ "hook_event_name": "StopFailure", "session_id": "c1", "error": error }).to_string();
        let mut child = sb
            .cmd()
            .args(["hook", "claude"])
            .env("ACA_SESSION_ID", id)
            .env("TMUX_PANE", &pane)
            .env("ACA_DELEGATE_DELAY", "0")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        pane
    };
    // A worker that was sent out, a transient error: say 「继续」 for it
    let pane = fail(&w, "API Error: 529 Overloaded");
    let log = || std::fs::read_to_string(sb.home.join("state/plugin-logs/aca.delegate.log")).unwrap_or_default();
    assert!(sb.wait_screen(&pane, "继续").contains("继续"), "{}\n{}", sb.tmux(&["capture-pane", "-p", "-t", &pane]), log());
    let log = log();
    assert!(log.contains("第 1 次代为发送「继续」"), "{log}");
    // Errors a retry won't fix, sessions you started yourself (no parent): left alone
    let other = sb.aca(&["new", "scr", &sb.proj(), "--parent", &lead]);
    let p2 = fail(&other, "prompt is too long");
    let p3 = fail(&mine, "API Error: 529 Overloaded");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    for p in [p2, p3] {
        assert!(!sb.tmux(&["capture-pane", "-p", "-t", &p]).contains("继续"));
    }
}

#[test]
fn a_codex_turn_that_ends_in_an_error_without_a_hook_is_found_in_its_rollout() {
    let Some(sb) = Sandbox::new("codexfail") else { return };
    sb.screen_tools();
    let lead = sb.aca(&["new", "fake", &sb.proj()]);
    let w = sb.aca(&["new", "fcx", &sb.proj(), "--parent", &lead, "--notify", "failed"]);
    let pane = sb.pane(&w);
    let hook = |json: &str| sb.hook_as("codex", &w, &pane, json);
    let rollout = sb.home.join("rollout.jsonl");
    let started = r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#;
    std::fs::write(&rollout, format!("{started}\n")).unwrap();
    hook(&serde_json::json!({"hook_event_name": "SessionStart", "session_id": "th-1", "transcript_path": rollout}).to_string());
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"改登录页"}"#);
    sb.set_screen(&w, &pane, "• Working (5s • esc to interrupt)");

    let child = sb.cmd().args(["wait", &w, "--timeout", "20"]).stdout(std::process::Stdio::piped()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    // Its own few retries don't help, and this turn ends with the error: no hooks run at all, just one entry in the record, and the
    // screen has stopped
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 1;
    let failed = format!(
        r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"t1","completed_at":{now},"error":{{"message":"stream disconnected before completion: Our servers are currently overloaded.","codex_error_info":{{"response_stream_disconnected":{{"http_status_code":null}}}}}}}}}}"#
    );
    std::fs::write(&rollout, format!("{started}\n{failed}\n")).unwrap();
    sb.set_screen(&w, &pane, "› ");
    // The one waiting on it can tell: an error, which kind, the original text (not passing off the last turn's reply as finished)
    let o = child.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string();
    assert_eq!(o.status.code(), Some(3), "{said}");
    assert!(said.contains("出错(临时错误") && said.contains("Our servers are currently overloaded"), "{said}");
    let is_failed = |l: &&str| l.contains(r#""event":"turn.failed""#);
    let ev = sb.aca(&["events", "-n", "100"]).lines().find(is_failed).unwrap().to_string();
    assert!(ev.contains("\"kind\":\"transient\""), "{ev}");
    // The one that sent it out asked for failed notices: got one, saying which kind
    let notice = sb.json(&["inbox", &lead, "--json"]);
    assert_eq!(notice[0]["what"], "turn.failed");
    assert!(notice[0]["text"].as_str().unwrap().contains("临时错误"), "{notice}");
    // Filled in only once: another look doesn't send it again
    sb.aca(&["ls"]);
    let all = sb.aca(&["events", "-n", "100"]);
    assert_eq!(all.lines().filter(is_failed).count(), 1, "{all}");
    // Saying something next (it started a new turn, and the record has the start): the error is cleared, running again
    std::fs::write(&rollout, format!("{started}\n{failed}\n{}\n", started.replace("t1", "t2"))).unwrap();
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"继续"}"#);
    sb.set_screen(&w, &pane, "• Working (1s • esc to interrupt)");
    let run = &sb.json(&["show", &w, "--json"])["run"];
    assert_eq!((run["status"].as_str(), run["error"].as_str(), run["error_kind"].as_str()), (Some("running"), Some(""), Some("")));
}

#[test]
fn a_codex_turn_that_finishes_without_its_stop_hook_is_found_in_its_rollout() {
    let Some(sb) = Sandbox::new("codexstop") else { return };
    sb.screen_tools();
    let lead = sb.aca(&["new", "fake", &sb.proj()]);
    let w = sb.aca(&["new", "fcx", &sb.proj(), "--parent", &lead, "--notify", "done"]);
    let pane = sb.pane(&w);
    let hook = |json: &str| sb.hook_as("codex", &w, &pane, json);
    let show = || sb.json(&["show", &w, "--json"]);
    let rollout = sb.home.join("rollout.jsonl");
    let started = r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#;
    std::fs::write(&rollout, format!("{started}\n")).unwrap();
    // This account trusts only the SessionStart and UserPromptSubmit hooks, not Stop: each turn reports only its start
    hook(&serde_json::json!({"hook_event_name": "SessionStart", "session_id": "th-1", "transcript_path": rollout}).to_string());
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"改登录页"}"#);
    sb.set_screen(&w, &pane, "• Working (5s • esc to interrupt)");
    assert_eq!(show()["state"], "running");
    // The screen has stopped but the record has no end yet (the moment right after stopping, or interrupted with Esc): not taken as
    // finished
    sb.set_screen(&w, &pane, "› ");
    assert!(sb.until(|| show()["state"] == "idle"), "{}", show());
    // Finished: Codex doesn't run the untrusted Stop hook and doesn't say so either, it just leaves an entry in the record
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let done = format!(
        r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"t1","completed_at":{at},"last_agent_message":"登录页改好了","error":null}}}}"#
    );
    std::fs::write(&rollout, format!("{started}\n{done}\n")).unwrap();
    // Anyone taking a look recognizes it: finished and unread, the last reply is the one in the record, and the finishing time is
    // when it really finished (not the moment of looking)
    let v = show();
    assert_eq!(
        (v["state"].as_str(), v["run"]["status"].as_str(), v["run"]["done"].as_u64()),
        (Some("unread"), Some("done"), Some(at)),
        "{v}"
    );
    assert_eq!(sb.aca(&["output", &w]), "登录页改好了");
    // The one that sent it out asked for done notices: got one
    let notice = sb.json(&["inbox", &lead, "--json"]);
    assert_eq!(notice[0]["what"], "turn.done", "{notice}");
    // Filled in only once: more looks don't send it again
    sb.aca(&["ls"]);
    sb.aca(&["ls"]);
    let is_done = |l: &&str| l.contains(r#""event":"turn.done""#);
    let all = sb.aca(&["events", "-n", "100"]);
    assert_eq!(all.lines().filter(is_done).count(), 1, "{all}");
    // Looked at, then saying something more: while the new turn has just started and the record has nothing yet, the last turn's
    // end doesn't count for this one
    sb.aca(&["seen", &w]);
    assert_eq!(show()["state"], "idle");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"再改注册页"}"#);
    let v = show();
    // For the first 3 seconds after reporting a start, trust the hook even with a quiet screen: running
    assert_eq!((v["state"].as_str(), v["run"]["status"].as_str()), (Some("running"), Some("running")), "{v}");
    let all = sb.aca(&["events", "-n", "100"]);
    assert_eq!(all.lines().filter(is_done).count(), 1, "{all}");
}

#[test]
fn a_worker_in_its_own_worktree_leaves_the_main_checkout_alone_and_rm_keeps_the_branch() {
    let Some(sb) = Sandbox::new("worktree") else { return };
    let repo = Path::new(&sb.proj()).canonicalize().unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    std::fs::write(repo.join("sub/a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    // Created in a subdirectory of the repo: the worktree opens under the repo's .worktrees/, the session starts in the same
    // subdirectory inside it, and the branch is named as given
    let o = sb.run(&["new", "fake", &repo.join("sub").to_string_lossy(), "-w", "feat/x"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let wt = repo.join(".worktrees/feat-x");
    assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("worktree {}(分支 feat/x)", wt.display())));
    assert_eq!(sb.json(&["show", &id, "--json"])["dir"], wt.join("sub").to_string_lossy().as_ref());
    assert_eq!((git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), git(&wt, &["rev-parse", "HEAD"])), ("feat/x".to_string(), head));
    // The main repo's git status doesn't show it
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    // Another one on the same branch: reuses that worktree
    let id2 = sb.aca(&["new", "fake", &repo.to_string_lossy(), "--worktree", "feat/x"]);
    assert_eq!(sb.json(&["show", &id2, "--json"])["dir"], wt.to_string_lossy().as_ref());

    // Another session is still in it: delete one, the worktree stays
    assert!(sb.aca(&["rm", &id]).contains("仍有其他会话在使用"));
    // Untracked files (probe scripts and the like): kept, no --force
    std::fs::write(wt.join("probe.sh"), "echo\n").unwrap();
    assert!(sb.aca(&["rm", &id2]).contains("已保留"));
    assert!(wt.is_dir());
    // Clean now: the worktree is deleted, the branch and its commits are all still there
    std::fs::remove_file(wt.join("probe.sh")).unwrap();
    std::fs::write(wt.join("b.txt"), "b\n").unwrap();
    git(&wt, &["add", "b.txt"]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    let id3 = sb.aca(&["new", "fake", &repo.to_string_lossy(), "-w", "feat/x"]);
    let said = sb.aca(&["rm", &id3]);
    assert!(said.contains("分支 feat/x 保留"), "{said}");
    assert!(!wt.exists());
    assert_eq!(git(&repo, &["log", "-1", "--format=%s", "feat/x"]), "work");

    // Not a repo, a bad branch name: not created
    let o = sb.run(&["new", "fake", &sb.home.join("config").to_string_lossy(), "-w", "x"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("不在 git 仓库中"));
    assert!(!sb.run(&["new", "fake", &repo.to_string_lossy(), "-w", "bad..name"]).status.success());
}

#[test]
fn interrupting_a_running_worker_presses_esc_only_for_the_one_that_sent_it_out() {
    let Some(sb) = Sandbox::new("interrupt") else { return };
    sb.screen_tools();
    let lead = sb.aca(&["new", "fake", &sb.proj()]);
    let other = sb.aca(&["new", "fake", &sb.proj()]);
    let w = sb.aca(&["new", "scr", &sb.proj(), "--parent", &lead]);
    let pane = sb.pane(&w);
    let err = |o: &Output| String::from_utf8_lossy(&o.stderr).to_string();
    // Not running: no key pressed
    sb.set_screen(&w, &pane, "❯ ");
    assert!(err(&sb.as_session(&lead, &["interrupt", &w], "")).contains("无需中止"));
    // Running: other sessions can't press it, the one that sent it out can
    sb.set_screen(&w, &pane, "✻ Pondering… (3s · ↓ 1k tokens)");
    sb.hook(&w, &pane, r#"{"hook_event_name":"UserPromptSubmit","session_id":"c1","prompt":"写一篇长文"}"#);
    assert!(err(&sb.as_session(&other, &["interrupt", &w], "")).contains("不是你派出的会话"));
    let o = sb.as_session(&lead, &["interrupt", &w], "");
    assert!(o.status.success(), "{}", err(&o));
    // What's pressed is Esc (the fake agent doesn't read keys, the terminal echoes it as ^[)
    assert!(sb.wait_screen(&pane, "^[").contains("^["));
    // Claude reports no hook when it stops: aca records it as idle itself, and the event says who pressed it
    assert_eq!(sb.json(&["show", &w, "--json"])["run"]["status"], "idle");
    let ev = sb.aca(&["events", "-n", "100"]).lines().find(|l| l.contains("turn.interrupted")).unwrap().to_string();
    assert!(ev.contains(&format!("\"by\":\"{lead}\"")), "{ev}");
}

#[test]
fn plugins_toml_says_which_plugins_with_what_settings_and_sync_makes_it_so() {
    let Some(sb) = Sandbox::new("decl") else { return };
    sb.screen_tools();
    // A local plugin: one setting, one capability with arguments, and an event hook that runs only for fake sessions
    let dir = sb.home.join("p1");
    write(
        &dir.join("aca-plugin.toml"),
        r#"id = "t.decl"
name = "Decl"
version = "0.1.0"
[settings.greeting]
default = "hi"
[[commands]]
id = "say"
title = "说一句"
args = { who = "string", n = "int?" }
command = ["sh", "-c", "echo \"$ACA_SETTING_GREETING $ACA_PLUGIN_ARGS_JSON\""]
[[events]]
on = "session.created"
if = { tool = "fake" }
command = ["sh", "-c", "echo \"$ACA_SETTING_GREETING\" >> \"$ACA_PLUGIN_STATE_DIR/seen\""]
"#,
    );
    let decl = sb.home.join("config/plugins.toml");
    // Link: plugins.toml records where it came from
    sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    let text = std::fs::read_to_string(&decl).unwrap();
    assert!(text.contains("[plugins.\"t.decl\"]") && text.contains(&dir.canonicalize().unwrap().to_string_lossy().into_owned()), "{text}");
    // The capability's arguments become JSON by the types in the manifest, and the settings are in the environment
    assert_eq!(sb.aca(&["plugin", "run", "t.decl.say", "who=me", "n=2"]), r#"hi {"n":2,"who":"me"}"#);
    let o = sb.run(&["plugin", "run", "t.decl.say", "n=2"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("who"));
    // A setting changed in plugins.toml: the new one applies next time
    std::fs::write(&decl, text.trim_end().to_string() + "\nsettings = { greeting = \"yo\" }\n").unwrap();
    assert!(sb.aca(&["plugin", "run", "t.decl.say", "who=me"]).starts_with("yo "));
    // The event hook's if: runs for sessions started by fake, not for those started by scr
    sb.aca(&["new", "fake", &sb.proj(), "--hold"]);
    sb.aca(&["new", "scr", &sb.proj(), "--hold"]);
    let seen = sb.home.join("state/plugins/t.decl/seen");
    let mut got = String::new();
    for _ in 0..50 {
        got = std::fs::read_to_string(&seen).unwrap_or_default();
        if !got.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(std::fs::read_to_string(&seen).unwrap_or_default(), "yo\n", "{got}");
    // Disabled from the command line: plugins.toml records it as disabled, with other lines and settings unchanged
    sb.aca(&["plugin", "disable", "t.decl"]);
    let text = std::fs::read_to_string(&decl).unwrap();
    assert!(text.contains("enabled = false") && text.contains("greeting = \"yo\""), "{text}");
    // Crossed out of plugins.toml: after sync it's gone locally too (the linked directory is untouched)
    let kept = text.lines().take_while(|l| !l.contains("t.decl")).collect::<Vec<_>>().join("\n");
    std::fs::write(&decl, kept + "\n").unwrap();
    assert!(sb.aca(&["plugin", "sync"]).contains("已移除 t.decl"));
    assert!(sb.aca(&["plugin", "ls"]).is_empty() && dir.join("aca-plugin.toml").is_file());
    // Written back: sync links it accordingly, and a disabled one stays disabled
    std::fs::write(&decl, format!("[plugins.\"t.decl\"]\nsource = \"{}\"\nenabled = false\n", dir.display())).unwrap();
    assert!(sb.aca(&["plugin", "sync"]).contains("已链接 t.decl"));
    assert!(sb.aca(&["plugin", "ls"]).starts_with("○ t.decl"));
    assert!(sb.aca(&["plugin", "sync"]).contains("无需更改"));
}

#[test]
fn a_plugin_mounts_its_directory_and_mcp_on_claude_and_codex_and_its_commands_become_tools() {
    let Some(sb) = Sandbox::new("mount") else { return };
    // A plugin: directories for both harnesses (one slash command), one MCP server, and one capability exposed to agents
    let dir = sb.home.join("both");
    write(
        &dir.join("aca-plugin.toml"),
        r#"id = "t.both"
name = "Both"
version = "0.1.0"
[agent]
dir = "agent"
[agent.mcp]
command = ["sh", "mcp.sh"]
[[commands]]
id = "hello"
title = "打个招呼"
args = { who = "string" }
expose = ["agent"]
command = ["sh", "-c", "echo \"hello $ACA_PLUGIN_ARGS_JSON from $ACA_SESSION_ID\""]
"#,
    );
    write(&dir.join("mcp.sh"), "echo \"mcp of $ACA_PLUGIN_ID for $ACA_SESSION_ID in $(pwd)\"\n");
    write(&dir.join("agent/.claude-plugin/plugin.json"), r#"{"name":"both"}"#);
    write(&dir.join("agent/commands/greet.md"), "---\ndescription: 打招呼\n---\n跟 $ARGUMENTS 打招呼\n");
    sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    // Two fake agents: started the Claude / Codex way, writing their arguments into a file, one per line; the Codex account has its
    // own config directory
    let codex_home = sb.home.join("codexhome");
    write(
        &sb.home.join("args.sh"),
        &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}/args-'\"$ACA_SESSION_ID\"\nexec sleep 600\n", sb.home.display()),
    );
    let cfg = sb.home.join("config/config.toml");
    let script = sb.home.join("args.sh");
    let text = std::fs::read_to_string(&cfg).unwrap()
        + &format!(
            "[tools.argc]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{s}\"]\n[tools.argx]\nkind = \"codex\"\ncmd = [\"/bin/sh\", \"{s}\"]\n[accounts.codex]\nt = \"{h}\"\n",
            s = script.display(),
            h = codex_home.display()
        );
    std::fs::write(&cfg, text).unwrap();
    let args_of = |id: &str| -> Vec<String> {
        let f = sb.home.join(format!("args-{id}"));
        for _ in 0..50 {
            if let Ok(t) = std::fs::read_to_string(&f) {
                return t.lines().map(String::from).collect();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("{id} 没起来");
    };
    let after = |args: &[String], flag: &str| -> Vec<String> { args.windows(2).filter(|w| w[0] == flag).map(|w| w[1].clone()).collect() };

    // Claude: the plugin directory via --plugin-dir, and two MCP servers (the plugin's own, and aca wrapping the capabilities) in one
    // --mcp-config
    let c = sb.aca(&["new", "argc", &sb.proj()]);
    let args = args_of(&c);
    assert!(after(&args, "--plugin-dir").contains(&dir.join("agent").canonicalize().unwrap().to_string_lossy().into_owned()), "{args:?}");
    let cfg_file = after(&args, "--mcp-config");
    assert_eq!(cfg_file.len(), 1, "{args:?}");
    let mcp: Value = serde_json::from_str(&std::fs::read_to_string(&cfg_file[0]).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["aca-t-both"]["args"], serde_json::json!(["plugin", "mcp", "t.both"]));
    assert_eq!(mcp["mcpServers"]["aca"]["args"], serde_json::json!(["plugin", "tools"]));

    // Codex: the plugin directory becomes a copy it recognizes, enabled per session; the plugin MCP and the capability MCP each also
    // become a copy holding only .mcp.json,
    // instead of going through -c mcp_servers (that way Codex can't produce a title), and the session id is passed through from
    // Codex's environment
    let x = sb.aca(&["new", "argx:t", &sb.proj()]);
    let flags = after(&args_of(&x), "-c");
    assert!(!flags.iter().any(|f| f.starts_with("mcp_servers.")), "{flags:?}");
    for p in ["t-both", "mcp-aca-t-both", "mcp-aca"] {
        assert!(flags.contains(&format!("plugins.{p}@aca.enabled=true")), "{p} {flags:?}");
    }
    let link = codex_home.join("plugins/cache/aca");
    assert_eq!(std::fs::read_link(&link).unwrap(), sb.home.join("data/codex-plugins"));
    let mcp: Value =
        serde_json::from_str(&std::fs::read_to_string(sb.home.join("data/codex-plugins/mcp-aca-t-both/local/.mcp.json")).unwrap()).unwrap();
    let server = &mcp["mcpServers"]["aca-t-both"];
    assert_eq!(server["args"], serde_json::json!(["plugin", "mcp", "t.both"]));
    assert!(server["env_vars"].as_array().unwrap().iter().any(|v| v == "ACA_SESSION_ID"), "{server}");
    let copy = sb.home.join("data/codex-plugins/t-both/local");
    let skill = std::fs::read_to_string(copy.join("skills/greet/SKILL.md")).unwrap();
    assert!(skill.starts_with("---\nname: greet\ndescription: 打招呼\n---\n"), "{skill}");
    assert!(std::fs::read_to_string(copy.join(".codex-plugin/plugin.json")).unwrap().contains("\"t-both\""));

    // Plugin disabled: new starts get nothing extra
    sb.aca(&["plugin", "disable", "t.both"]);
    let bare = sb.aca(&["new", "argc", &sb.proj()]);
    let args = args_of(&bare);
    assert!(!args.iter().any(|a| a == "--mcp-config" || a == "--plugin-dir"), "{args:?}");
    sb.aca(&["plugin", "enable", "t.both"]);

    // The MCP server wrapping capabilities: handshake, list the tools, call one (the session is the one in the agent's environment)
    let mut child = sb
        .cmd()
        .args(["plugin", "tools"])
        .env("ACA_SESSION_ID", &c)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        for m in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"t-both__hello","arguments":{"who":"me"}}}"#,
        ] {
            writeln!(stdin, "{m}").unwrap();
        }
    }
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    let replies: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 3, "{out}");
    assert_eq!(replies[1]["result"]["tools"][0]["name"], "t-both__hello");
    assert_eq!(replies[1]["result"]["tools"][0]["inputSchema"]["required"], serde_json::json!(["who"]));
    let said = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(said.trim(), format!(r#"hello {{"who":"me"}} from {c}"#));

    // The plugin's own MCP server: switch to the plugin directory, with the plugin's environment, and exec its command
    let o = sb.cmd().args(["plugin", "mcp", "t.both"]).env("ACA_SESSION_ID", &x).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert_eq!(said, format!("mcp of t.both for {x} in {}", dir.canonicalize().unwrap().display()));
}

/// One connection that speaks MCP with the gateway through the relay (for tests): stays open, one question, one answer
struct GwConn {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    rx: std::sync::mpsc::Receiver<Value>,
}

impl GwConn {
    fn open(sb: &Sandbox, session: &str) -> GwConn {
        let mut child = sb
            .cmd()
            .args(["plugin", "gateway", "connect"])
            .env("ACA_SESSION_ID", session)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    let _ = tx.send(v);
                }
            }
        });
        let mut c = GwConn { child, stdin, rx };
        c.ask(1, "initialize", serde_json::json!({ "protocolVersion": "2025-11-25" }));
        c.say(serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        c
    }

    fn say(&mut self, v: Value) {
        use std::io::Write;
        writeln!(self.stdin, "{v}").unwrap();
    }

    /// Ask one question and wait for the answer with the matching id (notifications in between are skipped)
    fn ask(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.say(serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let v = self.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap_or_else(|_| panic!("{method} 没回答"));
            if v["id"] == serde_json::json!(id) {
                return v;
            }
        }
    }

    fn tools(&mut self) -> Vec<String> {
        let r = self.ask(2, "tools/list", serde_json::json!({}));
        r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
    }

    fn call(&mut self, id: u64, name: &str) -> String {
        let r = self.ask(id, "tools/call", serde_json::json!({ "name": name, "arguments": {} }));
        r["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{r}")).trim().to_string()
    }

    fn close(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

#[test]
fn a_sessionless_gateway_client_can_list_and_call_plugins_with_agent_filters() {
    let Some(sb) = Sandbox::new("gw-unbound") else { return };
    let aca = env!("CARGO_BIN_EXE_aca");
    sb.aca(&["plugin", "install", "aca.gateway"]);
    for (id, capability) in [
        ("t.cmd", "[[commands]]\nid = \"ping\"\ntitle = \"Ping\"\nexpose = [\"agent\"]\ncommand = [\"echo\", \"pong\"]".into()),
        ("t.mcp", format!("[agent.mcp]\ncommand = [\"{aca}\", \"plugin\", \"tools\"]")),
    ] {
        let dir = sb.home.join(id);
        write(&dir.join("aca-plugin.toml"), &format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"0.1.0\"\n{capability}\n"));
        sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    }
    let decl = sb.home.join("config/plugins.toml");
    let mut text = std::fs::read_to_string(&decl).unwrap();
    for id in ["t.cmd", "t.mcp"] {
        let header = format!("[plugins.\"{id}\"]");
        assert!(text.contains(&header));
        text = text.replace(&header, &format!("{header}\nagents = [\"claude\"]"));
    }
    std::fs::write(decl, text).unwrap();

    let mut client = GwConn::open(&sb, "");
    let names = client.tools();
    let command = client.call(3, "t-cmd__ping");
    let mcp = client.call(4, "t-mcp__t-cmd__ping");
    client.close();
    sb.aca(&["plugin", "gateway", "stop"]);
    for name in ["t-cmd__ping", "t-mcp__t-cmd__ping"] {
        assert!(names.iter().any(|n| n == name), "{name} missing from {names:?}");
    }
    assert_eq!(command, "pong");
    assert_eq!(mcp, "pong");
}

#[test]
fn with_the_gateway_on_an_agent_mounts_one_server_and_a_shared_mcp_runs_once_for_everyone() {
    let Some(sb) = Sandbox::new("gw") else { return };
    let aca = env!("CARGO_BIN_EXE_aca");
    // The gateway plugin (from the repo), and two plugins: each has one capability exposed to agents, and both MCPs are aca's own
    // capability server (a real MCP server);
    // one shared, one per session
    let gwp = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/gateway").canonicalize().unwrap();
    sb.aca(&["plugin", "link", &gwp.to_string_lossy()]);
    for (id, scope) in [("t.gw", "shared"), ("t.ses", "session")] {
        let dir = sb.home.join(id);
        write(
            &dir.join("aca-plugin.toml"),
            &format!(
                "id = \"{id}\"\nname = \"{id}\"\nversion = \"0.1.0\"\n[agent.mcp]\ncommand = [\"{aca}\", \"plugin\", \"tools\"]\nscope = \"{scope}\"\n\
                 [[commands]]\nid = \"who\"\ntitle = \"谁在调\"\nexpose = [\"agent\"]\ncommand = [\"sh\", \"-c\", \"echo \\\"$ACA_SESSION_ID\\\"\"]\n"
            ),
        );
        sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    }
    // The gateway started in the test exits by itself after a minute with nobody connected
    let decl = sb.home.join("config/plugins.toml");
    let text = std::fs::read_to_string(&decl)
        .unwrap()
        .replace("[plugins.\"aca.gateway\"]", "[plugins.\"aca.gateway\"]\nsettings = { idle_minutes = 1 }");
    std::fs::write(&decl, text).unwrap();
    // A fake agent started the Claude way that writes its arguments into a file
    write(
        &sb.home.join("args.sh"),
        &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}/args-'\"$ACA_SESSION_ID\"\nexec sleep 600\n", sb.home.display()),
    );
    let cfg = sb.home.join("config/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap()
        + &format!("[tools.argc]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", sb.home.join("args.sh").display());
    std::fs::write(&cfg, text).unwrap();

    // The gateway is on: the agent gets only one aca, the relay to the gateway; the plugins' own MCPs are no longer attached
    // directly
    let a = sb.aca(&["new", "argc", &sb.proj()]);
    let b = sb.aca(&["new", "argc", &sb.proj()]);
    let args_file = sb.home.join(format!("args-{a}"));
    for _ in 0..50 {
        if args_file.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let args: Vec<String> = std::fs::read_to_string(&args_file).unwrap().lines().map(String::from).collect();
    let file = args.windows(2).find(|w| w[0] == "--mcp-config").map(|w| w[1].clone()).unwrap_or_else(|| panic!("{args:?}"));
    let mcp: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    let servers: Vec<&String> = mcp["mcpServers"].as_object().unwrap().keys().collect();
    assert_eq!(servers, ["aca"]);
    assert_eq!(mcp["mcpServers"]["aca"]["args"], serde_json::json!(["plugin", "gateway", "connect"]));

    // Two sessions connected at once: the same tools; on a call, the plugin command gets the session that called it
    let mut ca = GwConn::open(&sb, &a);
    let mut cb = GwConn::open(&sb, &b);
    let names = ca.tools();
    for want in ["t-gw__who", "t-ses__who", "t-gw__t-gw__who", "t-ses__t-ses__who"] {
        assert!(names.iter().any(|n| n == want), "{want}:{names:?}");
    }
    assert_eq!(cb.tools().len(), names.len());
    assert_eq!(ca.call(10, "t-gw__who"), a);
    assert_eq!(cb.call(11, "t-gw__who"), b);
    // The shared plugin MCP: just one, both sessions' calls go through it, and it knows who from _meta
    assert_eq!(ca.call(12, "t-gw__t-gw__who"), a);
    assert_eq!(cb.call(13, "t-gw__t-gw__who"), b);
    // The per-session one: each has its own process, and the environment holds its own session
    assert_eq!(cb.call(14, "t-ses__t-ses__who"), b);
    let st = sb.aca(&["plugin", "gateway", "status"]);
    assert!(st.contains("2 个连接"), "{st}");
    assert_eq!(st.matches("服务 t-gw:ready").count(), 1, "{st}");
    assert!(st.contains(&format!("服务 t-ses(会话 {a}):ready")) && st.contains(&format!("服务 t-ses(会话 {b}):ready")), "{st}");
    assert!(st.contains("✓ t-gw t-gw__who"), "{st}");
    // Turn the per-session one off for a only: a no longer sees it, b is unchanged; turned on again, it's back
    assert!(sb.aca(&["plugin", "gateway", "off", "t-ses", "--session", &a]).contains("不再"));
    assert!(!ca.tools().iter().any(|n| n == "t-ses__t-ses__who"));
    assert!(cb.tools().iter().any(|n| n == "t-ses__t-ses__who"));
    sb.aca(&["plugin", "gateway", "on", "t-ses", "--session", &a]);
    assert!(ca.tools().iter().any(|n| n == "t-ses__t-ses__who"));
    // Anything not in [mcps.*] can't be taken in
    let o = sb.run(&["plugin", "gateway", "add", "nope"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("[mcps.nope]"));
    // One session disconnects: its per-session one is shut down, the shared one stays
    cb.close();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let st = sb.aca(&["plugin", "gateway", "status"]);
    assert!(st.contains("1 个连接") && !st.contains(&format!("会话 {b}):ready")) && st.contains("服务 t-gw:ready"), "{st}");
    ca.close();
    // Stop: no process left behind
    assert!(sb.aca(&["plugin", "gateway", "stop"]).contains("已停止"));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(sb.aca(&["plugin", "gateway", "status"]).contains("未运行"));
}

#[test]
fn the_core_plugin_lets_an_agent_dispatch_a_worker_message_it_and_only_manage_its_own() {
    let Some(sb) = Sandbox::new("core") else { return };
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/core").canonicalize().unwrap();
    sb.aca(&["plugin", "link", &core.to_string_lossy()]);
    let lead = sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    let mine = sb.aca(&["new", "fake", &sb.proj(), "--parent", "-"]);
    // The lead's agent calls aca.core's tools through the capability server (the session in its environment is the lead)
    let mut child = sb
        .cmd()
        .args(["plugin", "tools"])
        .env("ACA_SESSION_ID", &lead)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str(&line) {
                let _ = tx.send(v);
            }
        }
    });
    let mut ask = |id: u64, method: &str, params: Value| -> Value {
        use std::io::Write;
        writeln!(stdin, "{}", serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(90)).unwrap_or_else(|_| panic!("{method} 没回答"))
    };
    ask(1, "initialize", serde_json::json!({ "protocolVersion": "2025-11-25" }));
    let tools: Vec<String> = ask(2, "tools/list", serde_json::json!({}))["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(tools.len(), 7, "{tools:?}");
    assert!(tools.contains(&"aca-core__dispatch".to_string()) && tools.contains(&"aca-core__interrupt".to_string()));
    let mut call = |id: u64, name: &str, args: Value| -> (bool, String) {
        let r = ask(id, "tools/call", serde_json::json!({ "name": name, "arguments": args }));
        (r["result"]["isError"].as_bool() == Some(true), r["result"]["content"][0]["text"].as_str().unwrap_or("").trim().to_string())
    };
    // Dispatch a worker: it goes under the lead, and the message got in too
    let (err, w) = call(3, "aca-core__dispatch", serde_json::json!({ "tool": "fake", "task": "看看登录页", "title": "读登录" }));
    assert!(!err, "{w}");
    let w = w.lines().last().unwrap().to_string();
    let (_, shown) = call(4, "aca-core__sessions", serde_json::json!({ "id": w }));
    let v: Value = serde_json::from_str(&shown).unwrap();
    assert_eq!((v["parent"].as_str(), v["title"].as_str()), (Some(lead.as_str()), Some("读登录")));
    // Pass a message: it went into its inbox
    let (err, said) = call(5, "aca-core__message", serde_json::json!({ "to": w, "text": "顺便看一下注册页" }));
    assert!(!err, "{said}");
    assert!(sb.aca(&["inbox", &w]).contains("顺便看一下注册页"));
    // Not one it sent out: can't interrupt it
    let (err, said) = call(6, "aca-core__interrupt", serde_json::json!({ "id": mine }));
    assert!(err, "{said}");
    // List sessions
    let (_, all) = call(7, "aca-core__sessions", serde_json::json!({}));
    assert!(all.contains(&lead) && all.contains(&w), "{all}");
    // Wake itself up in a while: the scheduled message goes into the lead's own inbox
    let (err, said) = call(8, "aca-core__wakeup", serde_json::json!({ "after": "10m", "text": "回来看看 CI" }));
    assert!(!err, "{said}");
    let inbox = sb.aca(&["inbox", &lead]);
    assert!(inbox.contains("[定时 ") && inbox.contains("回来看看 CI"), "{inbox}");
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn first_party_plugins_install_by_name_bring_what_they_need_and_running_sessions_hear_they_need_a_restart() {
    let Some(sb) = Sandbox::new("bundled") else { return };
    sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    // Started while there was no plugin: no restart needed
    assert!(!sb.aca(&["plugin", "ls"]).contains("主线"));
    // Built-in ones need no path; after installing it says what it brings
    let out = sb.aca(&["plugin", "install", "aca.delegate"]);
    assert!(out.contains("已安装 aca.delegate") && out.contains("提供给 agent:claude、codex 的插件目录"), "{out}");
    let copy = sb.home.join("data/first-party/delegate");
    assert!(copy.join("aca-plugin.toml").exists() && copy.join("claude/commands/delegate.md").exists());
    let decl = std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap();
    assert!(decl.contains("[plugins.\"aca.delegate\"]") && !decl.contains("source"), "{decl}");
    // It brings something to Claude: the open 「主线」 (the lead) didn't have it at start, so it needs a restart
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("aca.delegate") && ls.contains("aca 自带") && ls.contains("「主线」") && ls.contains("重启"), "{ls}");
    // A plugin that declares requires: the first-party plugins it needs are installed along with it
    let mine = sb.home.join("needs-core");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("aca-plugin.toml"), "id = \"my.needs\"\nname = \"N\"\nversion = \"1\"\nrequires = [\"aca.core\"]\n").unwrap();
    let out = sb.aca(&["plugin", "install", &mine.to_string_lossy()]);
    assert!(out.contains("aca.core") && sb.aca(&["plugin", "ls"]).contains("aca.core"), "{out}");
    // With only a table header in plugins.toml, sync installs built-in ones too
    let mut decl = std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap();
    decl.push_str("\n[plugins.\"aca.remote\"]\n");
    std::fs::write(sb.home.join("config/plugins.toml"), decl).unwrap();
    assert!(sb.aca(&["plugin", "sync"]).contains("已安装 aca.remote"));
    // A view: the command line prints its output
    assert!(sb.aca(&["plugin", "view", "aca.remote.tasks"]).contains("暂无远端任务"));
    assert!(sb.aca(&["plugin", "actions"]).contains("aca.remote.tasks  视图  远端任务"));
    // Remove: the copy aca wrote out is deleted too
    sb.aca(&["plugin", "remove", "aca.remote"]);
    assert!(!sb.home.join("data/first-party/remote").exists());
    // Installing a built-in one again: rewritten in place and registered again, no "already there" error
    let o = sb.run(&["plugin", "install", "aca.delegate"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn mcps_scattered_in_accounts_are_listed_and_taken_into_the_gateway_without_touching_the_accounts() {
    let Some(sb) = Sandbox::new("import") else { return };
    let gwp = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/gateway").canonicalize().unwrap();
    sb.aca(&["plugin", "link", &gwp.to_string_lossy()]);
    // A fake home directory: the default Claude, the registered claude:sub and the default Codex each have a few MCPs
    let home = sb.home.join("h");
    let gh = r#"{ "command": "npx", "args": ["-y", "gh"], "env": { "TOKEN": "s3cret" } }"#;
    write(
        &home.join(".claude.json"),
        &format!(r#"{{ "mcpServers": {{ "github": {gh}, "web": {{ "type": "http", "url": "https://x" }} }} }}"#),
    );
    write(&home.join("sub/.claude.json"), &format!(r#"{{ "mcpServers": {{ "github": {gh} }} }}"#));
    write(&home.join(".codex/config.toml"), "[mcp_servers.fs]\ncommand = \"fs-mcp\"\n");
    let cfg = sb.home.join("config/config.toml");
    let text =
        std::fs::read_to_string(&cfg).unwrap() + &format!("[accounts.claude]\nsub = {{ home = \"{}\" }}\n", home.join("sub").display());
    std::fs::write(&cfg, text).unwrap();
    let run = |args: &[&str]| {
        let o = sb.cmd().env("HOME", &home).args(args).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    let before = std::fs::read_to_string(home.join(".claude.json")).unwrap();
    // List only: same name with the same definition merged into one, HTTP ones marked; key values aren't printed
    let out = run(&["plugin", "gateway", "import"]);
    assert!(out.contains("github  [claude claude:sub]  npx -y gh  env TOKEN") && !out.contains("s3cret"), "{out}");
    assert!(
        out.contains("web  [claude]") && out.contains("HTTP") && out.contains("fs  [codex]  fs-mcp") && out.contains("--apply"),
        "{out}"
    );
    assert!(!std::fs::read_to_string(&cfg).unwrap().contains("[mcps."));
    // Take them in: written into aca's config.toml and the gateway's servers; the ones in accounts stay as they are
    let out = run(&["plugin", "gateway", "import", "--apply"]);
    assert!(out.contains("已导入 gateway:fs github") && out.contains("账号中的配置仍保留"), "{out}");
    let conf = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        conf.contains("[mcps.github]") && conf.contains("TOKEN = \"s3cret\"") && conf.contains("[mcps.fs]") && !conf.contains("[mcps.web]"),
        "{conf}"
    );
    assert!(std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("servers = [\"fs\", \"github\"]"));
    assert_eq!(std::fs::read_to_string(home.join(".claude.json")).unwrap(), before);
    // Once more: what's already there isn't written again
    run(&["plugin", "gateway", "import", "--apply"]);
    assert_eq!(std::fs::read_to_string(&cfg).unwrap().matches("[mcps.github]").count(), 1);
    // drop moves it out of the gateway
    assert!(run(&["plugin", "gateway", "drop", "fs"]).contains("不再通过 gateway"));
    assert!(std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("servers = [\"github\"]"));
}

/// A list view: aca plugin view prints one row per line, --json as is; the capability its key runs can be run from the view even
/// when it isn't exposed to the menu
#[test]
fn a_list_view_prints_as_rows_and_its_keys_commands_run_though_not_in_the_menu() {
    let Some(sb) = Sandbox::new("listview") else { return };
    sb.aca(&["plugin", "install", "aca.remote"]);
    let text = sb.aca(&["plugin", "view", "aca.remote.tasks"]);
    assert!(text.contains("尚未配置 server") && text.contains("暂无远端任务"), "{text}");
    let raw: Value = serde_json::from_str(&sb.aca(&["plugin", "view", "--json", "aca.remote.tasks"])).unwrap();
    assert_eq!(raw["rows"], serde_json::json!([]));
    // collect is exposed only to cli, not in the menu; the key r runs it
    let actions = sb.aca(&["plugin", "actions"]);
    assert!(actions.contains("aca.remote.tasks"), "{actions}");
    // --json: one capability per line (the global --json is taken out before dispatching to plugin, and still understood)
    let ids: Vec<String> = sb
        .aca(&["plugin", "actions", "--json"])
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["aca.remote.run", "aca.remote.new", "aca.remote.list", "aca.remote.rm", "aca.remote.collect", "aca.remote.check"]);
    assert!(sb.aca(&["plugin", "run", "aca.remote.collect"]).contains("已获取,没有新结果"));
    // aca mounts and aca remote moved into plugins: point the way
    let o = sb.run(&["mounts", "nope"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("已移到插件 aca.tailnet"));
    let o = sb.run(&["remote", "run", "gpu1", "--", "make"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("已移到插件 aca.remote"));
}

/// aca.remote lives entirely in its plugin (remote.sh, sh + jq + curl + ssh): run it through with a fake server (curl) and a
/// fake remote (ssh running locally) —
/// sent out, the remote finishes and calls back, fetched when the time comes, the result put in the inbox of the session that
/// sent it, the timer turned on and off again; lost, the session gone, registered outside a session,
/// the server unreachable, removed
#[test]
fn the_remote_plugin_sends_a_task_off_and_its_result_lands_in_the_sessions_inbox() {
    let Some(sb) = Sandbox::new("remote") else { return };
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("没有 jq,跳过");
        return;
    }
    let fake = sb.home.join("fake");
    let bin = fake.join("bin");
    for (name, body) in [("curl", include_str!("fake_remote/curl")), ("ssh", include_str!("fake_remote/ssh"))] {
        write(&bin.join(name), body);
        Command::new("chmod").arg("+x").arg(bin.join(name)).status().unwrap();
    }
    // PATH: the fakes first (the real curl and ssh are in /usr/bin); jq and tmux brought in separately
    let tools = fake.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    for prog in ["jq", "tmux"] {
        let at = String::from_utf8_lossy(&Command::new("sh").args(["-c", &format!("command -v {prog}")]).output().unwrap().stdout)
            .trim()
            .to_string();
        std::os::unix::fs::symlink(&at, tools.join(prog)).unwrap();
    }
    let path = format!("{}:{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display(), tools.display());
    let run = |args: &[&str]| {
        let o = sb.cmd().env("PATH", &path).env("FAKE", &fake).args(args).output().unwrap();
        (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    };
    let ok = |args: &[&str]| {
        let (good, out) = run(args);
        assert!(good, "aca {args:?}:{out}");
        out
    };
    let proj = sb.proj();
    let new_session = |title: &str| sb.json(&["new", "fake", &proj, "-t", title, "--hold", "--json"])["id"].as_str().unwrap().to_string();
    let lead = new_session("lead");
    let inbox = |id: &str| std::fs::read_to_string(sb.home.join(format!("state/inbox/{id}.jsonl"))).unwrap_or_default();
    let hub = fake.join("hub");
    let tasks = sb.home.join("state/plugins/aca.remote/tasks");
    let task = |id: &str| -> Value { serde_json::from_str(&std::fs::read_to_string(tasks.join(format!("{id}.json"))).unwrap()).unwrap() };
    let timer = || -> Value {
        let out = ok(&["plugin", "timer", "--json"]);
        out.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).find(|r| r["id"] == "aca.remote.collect").unwrap()
    };
    let set_hub = |id: &str, f: &dyn Fn(&mut Value)| {
        let file = hub.join(format!("{id}.json"));
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        f(&mut v);
        std::fs::write(&file, v.to_string()).unwrap();
    };
    sb.aca(&["plugin", "install", "aca.remote"]);

    // No server set up yet: say where to set it up
    let (good, out) = run(&["plugin", "run", "aca.remote.run", "--session", &lead, "host=gpu1", "command=true"]);
    assert!(!good && out.contains("尚未配置 server") && out.contains("aca plugin setting aca.remote url"), "{out}");
    sb.aca(&["plugin", "setting", "aca.remote", "url", "https://hook.test/"]);
    sb.aca(&["plugin", "setting", "aca.remote", "token", "right-token-0123456789"]);
    // The token is listed masked; the plugins.toml recording it is readable only by its owner
    assert!(!sb.aca(&["plugin", "setting", "aca.remote"]).contains("right-token"));
    let decl = std::fs::metadata(sb.home.join("config/plugins.toml")).unwrap();
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&decl.permissions()) & 0o777, 0o600);
    write(&fake.join("token"), "wrong-token-0123456789");
    let (good, out) = run(&["plugin", "run", "aca.remote.check"]);
    assert!(!good && out.contains("口令不正确"), "{out}");
    write(&fake.join("token"), "right-token-0123456789");
    let out = ok(&["plugin", "run", "aca.remote.check"]);
    assert!(out.contains("已连接,口令正确 · jobhook 0.1.0"), "{out}");

    // The machine can't be reached: say why, undo the registration just made (nothing left on the server or locally), the timer
    // isn't on
    let (good, out) = run(&["plugin", "run", "aca.remote.run", "--session", &lead, "host=nohost", "command=true"]);
    assert!(!good && out.contains("无法在 nohost 上启动:ssh: Could not resolve hostname nohost"), "{out}");
    assert_eq!(std::fs::read_dir(&hub).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&tasks).map(|d| d.count()).unwrap_or(0), 0);
    assert_eq!(timer()["on"], false);

    // Send it out: it starts in the background on the remote and returns at once; the timer is on
    let out = ok(&[
        "plugin",
        "run",
        "aca.remote.run",
        "--session",
        &lead,
        "host=gpu1",
        "command=echo 第一行; echo 最后一行; exit 3",
        "note=训练",
        "within=6h",
    ]);
    assert!(out.contains("已在 gpu1 后台跑起来") && out.contains("日志:gpu1:~/.aca-hooks/") && !out.contains("不在 aca 的会话中"), "{out}");
    let id =
        std::fs::read_dir(&tasks).unwrap().flatten().find_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(String::from)).unwrap();
    assert_eq!(id.len(), 32);
    let t = task(&id);
    assert_eq!((t["session"].as_str(), t["title"].as_str(), t["within"].as_i64()), (Some(lead.as_str()), Some("lead"), Some(21600)), "{t}");
    let tm = timer();
    assert_eq!((tm["on"].as_bool(), tm["scheduled"].as_bool()), (Some(true), Some(true)), "{tm}");
    assert!(ok(&["plugin", "run", "aca.remote.list"]).contains("gpu1 上的「训练」  等待中 · 已等"));
    // The remote finishes and calls back to the server (the fake remote runs in the background locally; wait for it)
    let rec = hub.join(format!("{id}.json"));
    assert!(sb.until(|| std::fs::read_to_string(&rec).is_ok_and(|t| t.contains("\"done\""))), "没回调");
    assert!(std::fs::read_to_string(fake.join(format!("remote/.aca-hooks/{id}.log"))).unwrap().contains("第一行"));

    // Fetch when the time comes: the result goes into lead's inbox (the sender is you), and is taken off the server; all settled,
    // so the timer turns off after this run
    assert_eq!(ok(&["plugin", "timer", "tick", "aca.remote.collect"]), "");
    let got = inbox(&lead);
    assert!(got.contains("[aca 远端任务] gpu1 上的「训练」结束了:失败了,退出码 3") && got.contains("最后一行"), "{got}");
    assert!(got.contains("\"from\":\"you\""), "{got}");
    assert!(!rec.exists());
    assert_eq!(task(&id)["state"], "sent");
    let tm = timer();
    assert_eq!((tm["on"].as_bool(), tm["scheduled"].as_bool(), tm["rc"].as_i64()), (Some(false), Some(false), Some(0)), "{tm}");
    assert!(ok(&["plugin", "log", "aca.remote"]).contains("gpu1 上的「训练」:已发送到会话"));
    // The list: ✕ exit code, ⏎ copies the command for viewing the log
    let v: Value = serde_json::from_str(&ok(&["plugin", "view", "--json", "aca.remote.tasks"])).unwrap();
    let row = &v["rows"][0];
    assert_eq!((row["mark"].as_str(), row["tone"].as_str()), (Some("✕"), Some("bad")), "{v}");
    assert!(row["right"].as_str().unwrap().starts_with("退出码 3 · ") && row["right"].as_str().unwrap().ends_with("已发送到会话"), "{v}");
    assert_eq!(row["fields"]["log_cmd"], format!("ssh gpu1 tail -n 50 ~/.aca-hooks/{id}.log"));
    assert!(row["detail"][0].as_str().unwrap().contains("会话「lead」"), "{v}");

    // Registered only (wiring up the callback yourself): going lost tells the session once; a callback arriving later is still sent
    let other = new_session("other");
    let out = ok(&["plugin", "run", "aca.remote.new", "--session", &other, "note=手动"]);
    let lost = out.lines().next().unwrap().strip_prefix("id ").unwrap().to_string();
    assert!(out.contains(&format!("'https://hook.test/h/{lost}?rc='$?")), "{out}");
    assert_eq!(timer()["on"], true);
    set_hub(&lost, &|v| v["state"] = "lost".into());
    assert!(ok(&["plugin", "run", "aca.remote.collect"]).contains("「手动」:失联"));
    assert!(inbox(&other).contains("「手动」过了 1d 还没有回调"));
    ok(&["plugin", "run", "aca.remote.collect"]);
    assert_eq!(inbox(&other).matches("还没有回调").count(), 1);
    assert!(ok(&["plugin", "run", "aca.remote.list"]).contains("失联 · 超过 1d 未回调"));
    set_hub(&lost, &|v| {
        v["state"] = "done".into();
        v["rc"] = 0.into();
        v["tail"] = "ok".into();
    });
    ok(&["plugin", "run", "aca.remote.collect"]);
    assert!(inbox(&other).contains("「手动」结束了:退出码 0"));

    // The session gone, registered outside a session: the result stays in the list, saying why
    let gone = new_session("gone");
    let id_gone = ok(&["plugin", "run", "aca.remote.new", "--session", &gone, "note=没人收"]).lines().next().unwrap()[3..].to_string();
    sb.aca(&["rm", &gone]);
    let out = ok(&["plugin", "run", "aca.remote.new", "note=外面"]);
    assert!(out.contains("不在 aca 的会话中"), "{out}");
    let id_out = out.lines().next().unwrap()[3..].to_string();
    for id in [&id_gone, &id_out] {
        set_hub(id, &|v| {
            v["state"] = "done".into();
            v["rc"] = 1.into();
        });
    }
    let out = ok(&["plugin", "run", "aca.remote.collect"]);
    assert!(out.contains("「没人收」:已结束,未送达(会话已不存在)") && out.contains("「外面」:已结束,未送达(登记时不在会话中)"), "{out}");
    assert_eq!(task(&id_gone)["state"], "kept");
    assert_eq!(timer()["on"], false);

    // The server unreachable: nothing fetched this time, not a failure; removing one that's waiting: deleted on the server too
    let waiting = ok(&["plugin", "run", "aca.remote.new", "--session", &lead, "note=等着"]).lines().next().unwrap()[3..].to_string();
    write(&fake.join("down"), "");
    let out = ok(&["plugin", "run", "aca.remote.collect"]);
    assert!(out.contains("本次未获取到:无法连接 server:(7) Failed to connect"), "{out}");
    let (good, out) = run(&["plugin", "run", "aca.remote.rm", &format!("id={}", &waiting[..6])]);
    assert!(!good && out.contains("无法连接 server"), "{out}");
    std::fs::remove_file(fake.join("down")).unwrap();
    // Another fetch is in progress: don't fetch this time; a lock held for very long is treated as unclaimed
    write(&tasks.join(".lock"), &format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()));
    assert!(ok(&["plugin", "run", "aca.remote.collect"]).contains("另一次获取正在进行"));
    write(&tasks.join(".lock"), "1000");
    assert!(!ok(&["plugin", "run", "aca.remote.collect"]).contains("另一次获取正在进行"));
    assert!(ok(&["plugin", "run", "aca.remote.rm", &format!("id={}", &waiting[..6])]).contains("已撤销 「等着」"));
    assert!(!hub.join(format!("{waiting}.json")).exists() && !tasks.join(format!("{waiting}.json")).exists());
    let (good, out) = run(&["plugin", "run", "aca.remote.rm", "id=zz"]);
    assert!(!good && out.contains("没有以 zz 开头的 id"), "{out}");
    assert_eq!(timer()["on"], false);
}

#[test]
fn timer_plist_upgrades_from_a_login_shell_and_survives_a_locale_free_tick() {
    let Some(sb) = Sandbox::new("timer-locale") else { return };
    let dir = sb.home.join("timed");
    write(
        &dir.join("aca-plugin.toml"),
        "id='t.locale'\nname='locale'\nversion='1'\n[[commands]]\nid='probe'\ncommand=['true']\n[[timers]]\nrun='probe'\nevery='1m'\n",
    );
    let q = "t.locale.probe";
    let cmd = |args: &[&str], vars: &[(&str, &str)]| {
        let mut c = sb.cmd();
        c.env_remove("LANG").env_remove("LC_ALL").env_remove("LC_CTYPE");
        let o = c.envs(vars.iter().copied()).args(args).output().unwrap();
        assert!(o.status.success(), "{args:?}: {o:?}");
    };
    let path = sb.home.join("state/timers/com.aca.timer.t-locale.probe.plist");
    let read = || std::fs::read_to_string(&path).unwrap();
    cmd(&["plugin", "link", dir.to_str().unwrap()], &[]);
    let old = read();
    assert!(!old.contains("<key>LANG</key>"));
    let all = [("LANG", "en_US.UTF-8"), ("LC_CTYPE", "UTF-8"), ("LC_ALL", "C")];
    cmd(&["plugin", "timer", "tick", q], &all);
    assert_eq!(read(), old);
    for args in [vec!["plugin", "timer", "on", q], vec!["plugin", "sync"]] {
        write(&path, &old);
        cmd(&args, &[]);
        assert_eq!(read(), old);
        cmd(&args, &[("LC_CTYPE", "UTF-8"), ("ACA_PLUGIN_TIMER", q)]);
        assert_eq!(read(), old);
        cmd(&args, &[("LC_CTYPE", "UTF-8")]);
        let partial = read();
        assert!(partial.contains("<key>LC_CTYPE</key>\n        <string>UTF-8</string>"));
        assert!(!partial.contains("<key>LANG</key>") && !partial.contains("<key>LC_ALL</key>"));
        cmd(&args, &all);
        let upgraded = read();
        for (k, v) in all {
            assert!(upgraded.contains(&format!("<key>{k}</key>\n        <string>{v}</string>")), "{upgraded}");
        }
        let modified = path.metadata().unwrap().modified().unwrap();
        cmd(&["plugin", "timer", "tick", q], &[]);
        cmd(&args, &[]);
        cmd(&args, &all);
        assert_eq!(read(), upgraded);
        assert_eq!(path.metadata().unwrap().modified().unwrap(), modified);
    }

    let state = sb.home.join("state/timers/t.locale.probe.json");
    let record = || serde_json::from_str::<Value>(&std::fs::read_to_string(&state).unwrap()).unwrap();
    let mut changed = record();
    changed["installed"] = 120.into();
    write(&state, &changed.to_string());
    write(&path, &old);
    cmd(&["plugin", "sync"], &all);
    assert_eq!(record(), changed);
    assert_eq!(read(), old);
    cmd(&["plugin", "timer", "on", q], &all);
    assert_eq!(record()["installed"], 60);
    assert!(read().contains("<key>LC_CTYPE</key>"));
}

/// Timers: the [[timers]] in the manifest; when the time comes, launchd runs aca plugin timer tick (the test doesn't touch
/// launchd: the plists are written in state/timers, and ticks are made here).
/// Ones without manual follow the plugin's on/off; manual ones are turned on and off by the plugin itself, and one turned off
/// during its own run is removed only after the run; a changed manifest or a deleted plugin gets cleaned up to match
#[test]
fn timers_run_when_due_follow_their_plugin_and_a_manual_one_switches_itself_off_after_its_run() {
    let Some(sb) = Sandbox::new("timers") else { return };
    let dir = sb.home.join("work/timed");
    let manifest = |every: &str, poll: bool| {
        let mut m = format!(
            "id = \"t.timed\"\nname = \"Timed\"\nversion = \"1\"\n\
             [[commands]]\nid = \"tick\"\nexpose = [\"cli\"]\ncommand = [\"sh\", \"tick.sh\"]\n\
             [[commands]]\nid = \"poll\"\nexpose = [\"cli\"]\ncommand = [\"sh\", \"poll.sh\"]\n\
             [[timers]]\nrun = \"tick\"\nevery = \"{every}\"\n"
        );
        if poll {
            m.push_str("[[timers]]\nrun = \"poll\"\nevery = \"1m\"\nmanual = true\n");
        }
        write(&dir.join("aca-plugin.toml"), &m);
    };
    manifest("10m", true);
    write(
        &dir.join("tick.sh"),
        "echo ran >> \"$ACA_PLUGIN_STATE_DIR/ticks\"\necho \"tick out\"\n[ -e \"$ACA_PLUGIN_STATE_DIR/fail\" ] && exit 3\nexit 0\n",
    );
    // Turns itself off after one run (like when all remote tasks are settled)
    write(&dir.join("poll.sh"), "echo polled >> \"$ACA_PLUGIN_STATE_DIR/polls\"\n\"$ACA_BIN\" plugin timer off \"$ACA_PLUGIN_ID.poll\"\n");
    let d = dir.to_str().unwrap();
    let out = sb.aca(&["plugin", "link", d]);
    assert!(!out.contains("提醒"), "{out}");
    let timers = sb.home.join("state/timers");
    let plist = |run: &str| timers.join(format!("com.aca.timer.t-timed.{run}.plist"));
    let count = |f: &str| std::fs::read_to_string(sb.home.join("state/plugins/t.timed").join(f)).unwrap_or_default().lines().count();
    let rows = || -> Vec<Value> { sb.aca(&["plugin", "timer", "--json"]).lines().map(|l| serde_json::from_str(l).unwrap()).collect() };

    // Linked: the one following the plugin is on (the plist runs this aca's tick), the manual one is off
    let ls = sb.aca(&["plugin", "timer"]);
    assert!(ls.contains("● t.timed.tick  每 10m · 已开启") && ls.contains("○ t.timed.poll  每 1m · 由插件自行开关 · 已关闭"), "{ls}");
    let p = std::fs::read_to_string(plist("tick")).unwrap();
    assert!(p.contains("<string>tick</string>\n        <string>t.timed.tick</string>") && p.contains("<integer>600</integer>"), "{p}");
    assert!(p.contains(env!("CARGO_BIN_EXE_aca")), "{p}");
    assert!(!plist("poll").exists());
    // The time comes: it runs once, the output goes to the plugin's log, the exit code is recorded
    let o = sb.run(&["plugin", "timer", "tick", "t.timed.tick"]);
    assert!(o.status.success() && o.stdout.is_empty(), "{o:?}");
    assert_eq!(count("ticks"), 1);
    let log = sb.aca(&["plugin", "log", "t.timed"]);
    assert!(log.contains(" timer tick\ntick out"), "{log}");
    // Just ran: crossing a whole second, or a slightly busy machine, makes it 1s or 2s ago
    let ls = sb.aca(&["plugin", "timer"]);
    assert!(["上次 0s 前 ✓", "上次 1s 前 ✓", "上次 2s 前 ✓"].iter().any(|s| ls.contains(s)), "{ls}");
    write(&sb.home.join("state/plugins/t.timed/fail"), "");
    sb.aca(&["plugin", "timer", "tick", "t.timed.tick"]);
    assert_eq!(rows()[0]["rc"], 3);
    assert!(sb.aca(&["plugin", "log", "t.timed"]).contains("(退出码 3)"));
    assert!(sb.aca(&["plugin", "timer"]).contains("✕ 退出码 3"));
    // One that follows the plugin can't be turned off
    let o = sb.run(&["plugin", "timer", "off", "t.timed.tick"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("随插件开关"), "{o:?}");

    // The manual one: on only when the plugin says so; during its own run it turns itself off and is removed after the run (the
    // plist gone, recorded as off), with how the last run went still recorded
    assert!(sb.aca(&["plugin", "timer", "on", "t.timed.poll"]).contains("已开启 t.timed.poll:每 1m 运行一次"));
    assert!(plist("poll").exists());
    sb.aca(&["plugin", "timer", "tick", "t.timed.poll"]);
    assert_eq!(count("polls"), 1);
    assert!(!plist("poll").exists());
    let r = rows().into_iter().find(|r| r["id"] == "t.timed.poll").unwrap();
    assert_eq!((r["on"].as_bool(), r["scheduled"].as_bool(), r["rc"].as_i64()), (Some(false), Some(false), Some(0)), "{r}");
    // An off one whose time comes (not fully removed on the launchd side yet): doesn't run
    sb.aca(&["plugin", "timer", "tick", "t.timed.poll"]);
    assert_eq!(count("polls"), 1);
    // run: run once now and print the output, even when off
    std::fs::remove_file(sb.home.join("state/plugins/t.timed/fail")).unwrap();
    assert!(sb.aca(&["plugin", "timer", "run", "t.timed.tick"]).contains("tick out"));
    assert_eq!(count("ticks"), 3);

    // Plugin disabled: all removed, and nothing runs when the time comes; enabled again, back as before
    sb.aca(&["plugin", "disable", "t.timed"]);
    assert!(!plist("tick").exists());
    assert!(sb.aca(&["plugin", "timer"]).contains("○ t.timed.tick  每 10m · 插件已关闭"));
    sb.aca(&["plugin", "timer", "tick", "t.timed.tick"]);
    assert_eq!(count("ticks"), 3);
    sb.aca(&["plugin", "enable", "t.timed"]);
    assert!(plist("tick").exists() && !plist("poll").exists());
    // Manifest changed: every changed, so reinstalled; a deleted timer is cleaned up
    manifest("20m", false);
    sb.aca(&["plugin", "link", d]);
    assert!(std::fs::read_to_string(plist("tick")).unwrap().contains("<integer>1200</integer>"));
    assert!(!timers.join("t.timed.poll.json").exists());
    assert_eq!(rows().len(), 1);
    // Plugin deleted: its timers are all gone
    sb.aca(&["plugin", "unlink", "t.timed"]);
    assert!(!plist("tick").exists() && !timers.join("t.timed.tick.json").exists());
    assert!(sb.aca(&["plugin", "timer"]).contains("没有插件设置定时任务"));
    // One already gone whose time comes: removes itself, runs nothing
    assert!(sb.run(&["plugin", "timer", "tick", "t.timed.tick"]).status.success());
    assert_eq!(count("ticks"), 3);
}

/// aca.tailnet lives entirely in its plugin (tailnet.sh, sh + jq): run listing, removing, mounting and publishing with a fake
/// tailscale / netstat / tpub.
/// aca only runs it and makes it speak the UI's language (ACA_LANG): ACA_LANG isn't set here, and config.toml says Chinese
#[test]
fn the_tailnet_plugin_does_mounts_on_its_own_through_the_plugin_interface() {
    let Some(sb) = Sandbox::new("tailnet") else { return };
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("没有 jq,跳过");
        return;
    }
    let fake = sb.home.join("fake");
    let (bin, pubbin) = (fake.join("bin"), fake.join("pubbin"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&pubbin).unwrap();
    std::fs::create_dir_all(sb.home.join("pub/docs")).unwrap();
    for (dir, name, body) in [
        (&bin, "tailscale", include_str!("fake_tailnet/tailscale")),
        (&bin, "netstat", include_str!("fake_tailnet/netstat")),
        (&pubbin, "tpub", include_str!("fake_tailnet/tpub")),
    ] {
        write(&dir.join(name), body);
        Command::new("chmod").arg("+x").arg(dir.join(name)).status().unwrap();
    }
    let docs = sb.home.join("pub/docs").canonicalize().unwrap();
    let serve = format!(
        r#"{{
      "TCP": {{"443": {{"HTTPS": true}}, "8443": {{"HTTPS": true}}, "8080": {{"HTTP": true}}, "2222": {{"TCPForward": "127.0.0.1:22"}}}},
      "Web": {{
        "mac.tail.ts.net:443": {{"Handlers": {{"/": {{"Proxy": "http://127.0.0.1:8420"}}}}}},
        "mac.tail.ts.net:8443": {{"Handlers": {{"/docs": {{"Path": "{}"}}, "/": {{"Proxy": "http://localhost:3000"}}}}}},
        "mac.tail.ts.net:8080": {{"Handlers": {{"/hi": {{"Text": "hello"}}}}}}
      }},
      "AllowFunnel": {{"mac.tail.ts.net:8443": true}},
      "Foreground": {{"abc": {{"TCP": {{"9000": {{"HTTPS": true}}}}, "Web": {{"mac.tail.ts.net:9000": {{"Handlers": {{"/": {{"Proxy": "http://127.0.0.1:9000"}}}}}}}}}}}}
    }}"#,
        docs.display()
    );
    // 3000 listens only on ::1, 22 on all addresses, 9000 only on the opening tailscale itself leaves, and nobody listens on 8420
    // (a connection doesn't count)
    let netstat = "Active Internet connections (including servers)
Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)
tcp4       0      0  127.0.0.1.8420         127.0.0.1.51234        ESTABLISHED
tcp6       0      0  ::1.3000               *.*                    LISTEN
tcp4       0      0  *.22                   *.*                    LISTEN
tcp6       0      0  *.22                   *.*                    LISTEN
tcp46      0      0  *.5173                 *.*                    LISTEN
tcp4       0      0  100.64.0.1.9000        *.*                    LISTEN
tcp6       0      0  fd7a:115c:a1e0::.9000  *.*                    LISTEN
";
    let reset = || {
        for f in ["log", "down", "off_ignored", "off_wipes", "pub_stuck"] {
            let _ = std::fs::remove_file(fake.join(f));
        }
        write(&fake.join("serve.json"), &serve);
        write(&fake.join("netstat.txt"), netstat);
        write(&fake.join("status.json"), r#"{"BackendState": "Running", "Self": {"DNSName": "mac.tail.ts.net."}}"#);
        write(&fake.join("tpub"), "off");
    };
    let cfg = sb.home.join("config/config.toml");
    write(&cfg, &format!("{}\n[ui]\nlang = \"zh\"\n", std::fs::read_to_string(&cfg).unwrap()));
    // PATH holds only the fakes and system directories: not one of the real tailscale and tpub on the machine (in ~/.local/bin,
    // /opt/homebrew/bin) can be found,
    // so testing remove and publish can't touch real mounts. jq is brought in from a directory of its own
    let tools = fake.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let jq = String::from_utf8_lossy(&Command::new("sh").args(["-c", "command -v jq"]).output().unwrap().stdout).trim().to_string();
    std::os::unix::fs::symlink(&jq, tools.join("jq")).unwrap();
    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", tools.display());
    let run = |with_tpub: bool, args: &[&str]| {
        let p = if with_tpub { format!("{}:{}:{path}", bin.display(), pubbin.display()) } else { format!("{}:{path}", bin.display()) };
        let o = sb.cmd().env_remove("ACA_LANG").env("PATH", p).env("FAKE", &fake).args(args).output().unwrap();
        (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    };
    let did = || std::fs::read_to_string(fake.join("log")).unwrap_or_default();
    sb.aca(&["plugin", "install", "aca.tailnet"]);
    // As if aca had just been upgraded: what was written out is still the old one (calling the gone aca mounts). Before running it,
    // it's replaced with the new one
    write(&sb.home.join("data/first-party/tailnet/tailnet.sh"), "#!/bin/sh\nexec \"$ACA_BIN\" mounts --json\n");
    reset();

    // The list view: one row per line, whether the target is alive, public / foreground marked; the publish switch on top
    let (ok, out) = run(true, &["plugin", "view", "--json", "aca.tailnet.mounts"]);
    assert!(ok, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["head"], "mac.tail.ts.net\n发布 已关闭 · 2 项");
    assert_eq!(v["fields"]["publish_to"], "on");
    let row = |id: &str| v["rows"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap_or_else(|| panic!("没有 {id}:{v}")).clone();
    let r = row("https:443/");
    assert_eq!((r["mark"].as_str(), r["tone"].as_str(), r["right"].as_str()), (Some("✕"), Some("bad"), Some("未监听")));
    assert_eq!(r["text"], ":443 /         → 127.0.0.1:8420");
    assert_eq!((r["fields"]["url"].as_str(), r["fields"]["drop"].as_str()), (Some("https://mac.tail.ts.net/"), Some("https:443/")));
    assert_eq!(row("https:8443/")["right"], "监听中 · 公网");
    assert_eq!(row("https:8443/docs")["right"], "文件存在 · 公网");
    assert_eq!(row("tcp:2222")["right"], "监听中");
    let fg = row("fg:https:9000/");
    assert!(fg["fields"].get("drop").is_none() && fg["right"] == "未监听 · 前台", "{fg}");
    // The one for people / agents to read
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.list"]);
    assert!(ok && out.starts_with("mac.tail.ts.net\n✕ https://mac.tail.ts.net/ "), "{out}");
    assert!(out.contains("● https://mac.tail.ts.net:8443/docs  → ") && out.contains("文件存在 · 公网"), "{out}");

    // Remove: passes the port and path; one open to the internet is removed with funnel; afterwards it writes how to add it back
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.drop", "target=https:8443/docs"]);
    assert!(
        ok && out.contains("已撤销 https://mac.tail.ts.net:8443/docs")
            && out.contains("如需加回:tailscale funnel --bg --https=8443 --set-path=/docs"),
        "{out}"
    );
    run(false, &["plugin", "run", "aca.tailnet.drop", "target=https://mac.tail.ts.net/"]);
    run(false, &["plugin", "run", "aca.tailnet.drop", "target=tcp:2222"]);
    assert_eq!(did(), "funnel --https=8443 --set-path=/docs off\nserve --https=443 --set-path=/ off\nserve --tcp=2222 off\n");
    // One that isn't there, a foreground one: no command is typed
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.drop", "target=https:443/"]);
    assert!(!ok && out.contains("没有挂载 https:443/"), "{out}");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.drop", "target=fg:https:9000/"]);
    assert!(!ok && out.contains("前台挂载无法撤销"), "{out}");
    assert_eq!(did().lines().count(), 3);
    // tailscale says it removed one but didn't; removing one took others with it: both reported as they are, with how to add back
    // the ones taken along
    reset();
    write(&fake.join("off_ignored"), "");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.drop", "target=https:443/"]);
    assert!(!ok && out.contains("仍在挂载中") && out.contains("tailscale serve --https=443 --set-path=/ off"), "{out}");
    reset();
    write(&fake.join("off_wipes"), "");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.drop", "target=https:443/"]);
    assert!(!ok && out.contains("也一起消失了") && out.contains("tailscale serve --bg --tcp=2222 tcp://127.0.0.1:22"), "{out}");
    assert!(out.contains("http://mac.tail.ts.net:8080/hi") && !out.contains("9000"), "{out}");

    // Mount: the same port number on the tailnet; a target not listening gets a mention; a port number already taken is left alone
    reset();
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.add", "port=3000"]);
    assert!(ok && out.trim() == "已挂载 https://mac.tail.ts.net:3000/", "{out}");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.add", "port=5174"]);
    assert!(ok && out.contains("本机 5174 当前未监听"), "{out}");
    for taken in ["443", "2222", "9000"] {
        let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.add", &format!("port={taken}")]);
        assert!(!ok && out.contains("已有其他挂载"), "{taken}: {out}");
    }
    assert_eq!(did(), "serve --bg --https=3000 http://localhost:3000\nserve --bg --https=5174 http://localhost:5174\n");
    // tailscale not running: the list still comes out, saying why; can't mount
    reset();
    write(&fake.join("down"), "");
    let (ok, out) = run(false, &["plugin", "view", "--json", "aca.tailnet.mounts"]);
    assert!(ok && out.contains("is Tailscale running"), "{out}");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.add", "port=3000"]);
    assert!(!ok && out.contains("is Tailscale running"), "{out}");

    // Publish: what tpub says is passed on as is; saying on while actually not on doesn't count
    reset();
    write(&fake.join("tpub"), "busy");
    let (ok, out) = run(true, &["plugin", "run", "aca.tailnet.publish", "to=on"]);
    assert!(!ok && out.contains("无法开启:tailnet 的 :8443 上挂着别的东西"), "{out}");
    write(&fake.join("tpub"), "off");
    write(&fake.join("pub_stuck"), "");
    let (ok, out) = run(true, &["plugin", "run", "aca.tailnet.publish", "to=on"]);
    assert!(!ok && out.contains("已请求 tpub 开启,但当前状态为:已关闭 · 2 项"), "{out}");
    let _ = std::fs::remove_file(fake.join("pub_stuck"));
    let (ok, out) = run(true, &["plugin", "run", "aca.tailnet.publish", "to=on"]);
    assert!(ok && out.contains("发布已打开"), "{out}");
    let (_, out) = run(true, &["plugin", "view", "--json", "aca.tailnet.mounts"]);
    assert!(out.contains("发布 已开启 · 2 项") && out.contains(r#""publish_to":"off""#), "{out}");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.publish", "to=on"]);
    assert!(!ok && out.contains("未安装 tpub"), "{out}");
    // tpub not installed: no publish line, p can't be pressed
    let (_, out) = run(false, &["plugin", "view", "--json", "aca.tailnet.mounts"]);
    assert!(!out.contains("发布") && !out.contains("publish_to"), "{out}");
}

/// Writing a plugin from scratch: right after new it can be linked and run; check works without linking; emit runs an event hook
/// in the foreground and prints its output
#[test]
fn a_new_plugin_links_and_runs_check_names_problems_and_emit_runs_hooks_in_the_foreground() {
    let Some(sb) = Sandbox::new("plugdev") else { return };
    let dir = sb.home.join("work/hello");
    let d = dir.to_str().unwrap();
    let out = sb.aca(&["plugin", "new", d]);
    assert!(out.contains("(me.hello)") && out.contains("aca plugin run me.hello.hello"), "{out}");
    assert!(sb.aca(&["plugin", "check", d]).contains("✓ 未发现问题"));
    // A directory that already has things in it is left alone
    assert!(!sb.run(&["plugin", "new", d]).status.success());
    sb.aca(&["plugin", "link", d]);
    assert!(sb.aca(&["plugin", "run", "me.hello.hello"]).contains("你好!会话 (没有)"));
    // The hook is still commented out: nobody subscribes
    assert!(sb.aca(&["plugin", "emit", "turn.done"]).contains("没有已开启的插件订阅 turn.done"));
    let m = dir.join("aca-plugin.toml");
    let text = std::fs::read_to_string(&m).unwrap();
    write(&m, &text.replace("# [[events]]\n# on = \"turn.done\"\n# command = [\"sh\", \"hello.sh\"]", "[[events]]\non = \"turn.done\"\ncommand = [\"sh\", \"-c\", \"echo $ACA_PLUGIN_EVENT $ACA_PLUGIN_EVENT_JSON\"]\n\n[[events]]\non = \"turn.failed\"\nif = { kind = \"transient\" }\ncommand = [\"sh\", \"-c\", \"echo again; exit 3\"]"));
    sb.aca(&["plugin", "link", d]);
    // The event's fields go into that line by type; --plugin runs only that one
    let out = sb.aca(&["plugin", "emit", "turn.done", "secs=12", "last=好了", "--plugin", "me.hello"]);
    assert!(out.contains("--- me.hello · sh -c echo") && out.contains("退出码 0"), "{out}");
    assert!(out.contains(r#""event":"turn.done""#) && out.contains(r#""secs":12"#) && out.contains(r#""last":"好了""#), "{out}");
    // One whose if doesn't match doesn't run; when one matches and exits nonzero, emit's exit code isn't 0 either
    assert!(sb.aca(&["plugin", "emit", "turn.failed", "kind=account"]).contains("没有已开启的插件订阅"));
    let o = sb.run(&["plugin", "emit", "turn.failed", "kind=transient"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success() && out.contains("退出码 3") && out.contains("again"), "{out}");
    assert!(!sb.run(&["plugin", "emit", "turn.nope"]).status.success());
    // check: a missing script, a missing program it needs, each named; with problems the exit code is 1
    let bad = sb.home.join("work/bad");
    std::fs::create_dir_all(&bad).unwrap();
    write(
        &bad.join("aca-plugin.toml"),
        "id = \"x.bad\"\nname = \"Bad\"\nversion = \"1\"\nneeds = [\"surely-missing-prog-aca\"]\n[[commands]]\nid = \"a\"\ncommand = [\"sh\", \"gone.sh\"]\n",
    );
    let o = sb.run(&["plugin", "check", bad.to_str().unwrap()]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        !o.status.success() && out.contains("✕ 命令 a:插件目录中没有 gone.sh") && out.contains("⚠ 需要 surely-missing-prog-aca"),
        "{out}"
    );
    // Linking also warns about missing programs; aca plugin ls says so too
    let out = sb.aca(&["plugin", "link", bad.to_str().unwrap()]);
    assert!(out.contains("需要 surely-missing-prog-aca"), "{out}");
    assert!(sb.aca(&["plugin", "ls"]).contains("⚠ x.bad:需要 surely-missing-prog-aca"));
}

/// The contract of the plugin interface (api 1): each way of calling hands the command exactly these environment variables, all
/// documented; a list view reads only stdout;
/// unknown fields in the manifest get a warning, one for a newer interface than aca's can't be linked, one whose name clashes on
/// the agent side can't be linked
#[test]
fn the_plugin_contract_gives_each_kind_of_call_its_documented_variables_and_guards_the_manifest() {
    let Some(sb) = Sandbox::new("contract") else { return };
    let dir = sb.home.join("work/contract");
    write(
        &dir.join("aca-plugin.toml"),
        r#"id = "t.contract"
name = "Contract"
version = "1"
api = 1
[settings.max-lines]
default = 3
[[commands]]
id = "env"
title = "交给它的环境"
args = { word = "string?" }
expose = ["menu", "cli", "agent"]
command = ["sh", "env.sh"]
[[views]]
id = "envview"
title = "环境"
command = ["sh", "env.sh"]
[[views]]
id = "rows"
title = "列表"
rows = true
command = ["sh", "rows.sh"]
[[events]]
on = "turn.done"
command = ["sh", "env.sh"]
[[timers]]
run = "env"
every = "5m"
manual = true
"#,
    );
    // What comes in from outside (ACA_HOME and ACA_TMUX_SOCKET the test gives aca) doesn't count: plugin processes inherit aca's
    // environment anyway
    write(
        &dir.join("env.sh"),
        "env | sed -n 's/^\\(ACA_[A-Z0-9_]*\\)=.*/\\1/p' | grep -v -e '^ACA_HOME$' -e '^ACA_TMUX_SOCKET$' | sort -u | tr '\\n' ' '\necho\necho \"parent=$ACA_SESSION_PARENT\"\necho \"by=$ACA_INVOKED_BY timer=$ACA_PLUGIN_TIMER\"\n",
    );
    write(
        &dir.join("rows.sh"),
        "echo 'a note on stderr' >&2\necho '{\"head\": \"h\", \"rows\": [{\"id\": \"a\", \"text\": \"第一条\"}]}'\n",
    );
    let d = dir.to_str().unwrap();
    let out = sb.aca(&["plugin", "link", d]);
    assert!(!out.contains("提醒"), "{out}");
    let proj = sb.proj();
    let lead = sb.json(&["new", "fake", &proj, "-t", "lead", "--hold", "--json"])["id"].as_str().unwrap().to_string();
    let o = sb.as_session(&lead, &["new", "fake", &proj, "-t", "worker", "--hold", "--json"], "");
    let worker = serde_json::from_slice::<Value>(&o.stdout).unwrap()["id"].as_str().unwrap().to_string();

    let always = [
        "ACA_BIN",
        "ACA_ENV",
        "ACA_INVOKED_BY",
        "ACA_LANG",
        "ACA_PLUGIN_CONFIG_DIR",
        "ACA_PLUGIN_CONTEXT_JSON",
        "ACA_PLUGIN_ID",
        "ACA_PLUGIN_ROOT",
        "ACA_PLUGIN_SETTINGS_JSON",
        "ACA_PLUGIN_STATE_DIR",
        "ACA_PLUGIN_TIMER",
        "ACA_SESSION_DIR",
        "ACA_SESSION_ID",
        "ACA_SESSION_PANE",
        "ACA_SESSION_PARENT",
        "ACA_SETTING_MAX_LINES",
    ];
    let want = |more: &[&str]| {
        let mut v: Vec<String> = always.iter().chain(more).map(|s| s.to_string()).collect();
        v.sort();
        v
    };
    let names = |out: &str| -> Vec<String> {
        let line = out.lines().find(|l| l.contains("ACA_BIN")).unwrap_or_else(|| panic!("{out}"));
        line.split_whitespace().map(String::from).collect()
    };
    let command = ["ACA_ARG_WORD", "ACA_PLUGIN_ACTION_ID", "ACA_PLUGIN_ARGS_JSON", "ACA_PLUGIN_ENTRYPOINT_ID"];
    // A capability run from the command line: aimed at the worker, and the one that sent it out is in ACA_SESSION_PARENT
    let out = sb.aca(&["plugin", "run", "t.contract.env", "--session", &worker, "word=hi"]);
    assert_eq!(names(&out), want(&command), "{out}");
    assert!(out.contains(&format!("parent={lead}")), "{out}");
    // A view
    let out = sb.aca(&["plugin", "view", "t.contract.envview"]);
    assert_eq!(names(&out), want(&["ACA_PLUGIN_ENTRYPOINT_ID"]), "{out}");
    // An event hook
    let out = sb.aca(&["plugin", "emit", "turn.done", "--plugin", "t.contract", "--session", &worker]);
    assert_eq!(names(&out), want(&["ACA_PLUGIN_EVENT", "ACA_PLUGIN_EVENT_JSON"]), "{out}");
    assert!(out.contains(&format!("parent={lead}")), "{out}");
    assert!(out.contains("by=event timer=\n") || out.ends_with("by=event timer="), "{out}");
    // A timer: the same environment as running this capability from the command line, no session, and ACA_PLUGIN_TIMER says which
    // timer
    let out = sb.aca(&["plugin", "timer", "run", "t.contract.env"]);
    assert_eq!(names(&out), want(&command[1..]), "{out}");
    assert!(out.contains("parent=\n") && out.contains("by=timer timer=t.contract.env"), "{out}");
    // A tool called by an agent (MCP)
    let mut child = sb
        .cmd()
        .args(["plugin", "tools"])
        .env("ACA_SESSION_ID", &worker)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        for m in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"t-contract__env","arguments":{}}}"#,
        ] {
            writeln!(stdin, "{m}").unwrap();
        }
    }
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    let reply: Value = serde_json::from_str(out.lines().last().unwrap()).unwrap();
    let text = reply["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{out}"));
    assert_eq!(names(text), want(&command[1..]), "{text}");
    assert!(text.contains(&format!("parent={lead}")), "{text}");
    // Every one seen above is in the docs (the English plugins.md, the Chinese plugins.zh-CN.md) under the environment handed to
    // commands
    for file in ["plugins.md", "plugins.zh-CN.md"] {
        let docs = std::fs::read_to_string(format!("{}/../../docs/{file}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        for n in want(&["ACA_PLUGIN_EVENT", "ACA_PLUGIN_EVENT_JSON", "ACA_INPUT_FILE"])
            .iter()
            .chain(command.iter().map(|s| s.to_string()).collect::<Vec<_>>().iter())
        {
            let n = n.strip_prefix("ACA_ARG_").map(|_| "ACA_ARG_<").or(n.strip_prefix("ACA_SETTING_").map(|_| "ACA_SETTING_<")).unwrap_or(n);
            assert!(docs.contains(&format!("`{n}")), "docs/{file} 没写 {n}");
        }
    }

    // A list view runs reading only stdout: stderr goes into the plugin's log, not mixed into the JSON
    let out = sb.aca(&["plugin", "view", "t.contract.rows"]);
    assert!(out.contains("第一条") && !out.contains("a note"), "{out}");
    let raw = sb.json(&["plugin", "view", "t.contract.rows", "--json"]);
    assert_eq!(raw["rows"][0]["id"], "a");
    assert!(sb.aca(&["plugin", "log", "t.contract"]).contains("a note on stderr"));

    // Unknown fields in the manifest: linking still links, with a warning naming them (a needs written after [[keys]] belongs to
    // that key)
    let typo = sb.home.join("work/typo");
    write(
        &typo.join("aca-plugin.toml"),
        "id = \"t.typo\"\nname = \"Typo\"\nversion = \"1\"\ndescripton = \"x\"\n[[commands]]\nid = \"go\"\ncommand = [\"true\"]\n[[keys]]\nkey = \"g\"\nrun = \"go\"\nneeds = [\"sh\"]\n",
    );
    let out = sb.aca(&["plugin", "link", typo.to_str().unwrap()]);
    assert!(out.contains("无法识别的 descripton") && out.contains("无法识别的 keys[1].needs"), "{out}");
    assert!(sb.aca(&["plugin", "check", typo.to_str().unwrap()]).contains("⚠ 清单中有无法识别的 keys[1].needs"));
    // Written for a newer interface version: can't be linked, says to upgrade aca first
    let newer = sb.home.join("work/newer");
    write(&newer.join("aca-plugin.toml"), "id = \"t.newer\"\nname = \"Newer\"\nversion = \"1\"\napi = 2\n");
    let o = sb.run(&["plugin", "link", newer.to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("先升级 aca"), "{o:?}");
    // An id that, turned into its agent-side name, clashes with one already linked (t-contract and t.contract are both t-contract):
    // can't be linked
    let twin = sb.home.join("work/twin");
    write(&twin.join("aca-plugin.toml"), "id = \"t-contract\"\nname = \"Twin\"\nversion = \"1\"\n");
    let o = sb.run(&["plugin", "link", twin.to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("同名"), "{o:?}");
}

/// A window you opened in some session's tmux: show / output / plugin run without an id are aimed at that session.
/// It only fills in an id that wasn't given: not for an agent itself (ACA_SESSION_ID in its environment), and new sessions don't
/// go under it because of this
#[test]
fn a_window_you_opened_in_a_sessions_tmux_needs_no_session_id() {
    let Some(sb) = Sandbox::new("here") else { return };
    let proj = sb.proj();
    let id = sb.json(&["new", "fake", &proj, "-t", "here", "--json"])["id"].as_str().unwrap().to_string();
    let other = sb.json(&["new", "fake", &proj, "-t", "other", "--json"])["id"].as_str().unwrap().to_string();
    let tmux = sb.json(&["show", &id, "--json"])["tmux"].as_str().unwrap().to_string();
    let mine = sb.tmux(&["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", &format!("{tmux}:")]);
    assert!(mine.starts_with('%'), "{mine}");
    let typed = |pane: &str, agent: Option<&str>, args: &[&str]| {
        let mut c = sb.cmd();
        c.args(args).env("TMUX", "/tmp/aca-e2e-fake,1,0").env("TMUX_PANE", pane);
        if let Some(a) = agent {
            c.env("ACA_SESSION_ID", a);
        }
        c.output().unwrap()
    };
    let o = typed(&mine, None, &["show", "--json"]);
    assert!(o.status.success(), "{o:?}");
    assert_eq!(serde_json::from_slice::<Value>(&o.stdout).unwrap()["id"], id.as_str());
    assert!(typed(&mine, None, &["output"]).status.success());
    // Plugin commands are aimed at it too
    let dir = sb.home.join("work/here");
    write(
        &dir.join("aca-plugin.toml"),
        "id = \"t.here\"\nname = \"Here\"\nversion = \"1\"\n[[commands]]\nid = \"who\"\ncommand = [\"sh\", \"-c\", \"echo session=$ACA_SESSION_ID\"]\n",
    );
    sb.aca(&["plugin", "link", dir.to_str().unwrap()]);
    let o = typed(&mine, None, &["plugin", "run", "t.here.who"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), format!("session={id}"));
    // A new session doesn't go under this session ("who am I" still looks only at ACA_SESSION_ID)
    let o = typed(&mine, None, &["new", "fake", &proj, "-t", "child", "--hold", "--json"]);
    assert_eq!(serde_json::from_slice::<Value>(&o.stdout).unwrap()["parent"], "", "{o:?}");
    // Called by an agent itself (ACA_SESSION_ID in its environment): not filled in, no id given means none given
    assert!(!typed(&mine, Some(&other), &["show"]).status.success());
    // Not one of aca's tmux sessions: nothing to fill in
    let plain = sb.tmux(&["new-session", "-d", "-s", "plain", "-P", "-F", "#{pane_id}"]);
    assert!(!typed(&plain, None, &["show"]).status.success());
}

/// Claude's name follows the session (`aca hook-name`): a named one hands it over at start, and says nothing more when it's
/// already called that;
/// renamed in aca, it follows; once /rename was used in Claude, it's left alone; other events and non-aca sessions write not a
/// single character
#[test]
fn claude_gets_the_session_name_as_its_own() {
    let Some(sb) = Sandbox::new("name") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "修登录"]);
    let ask = |session: &str, ev: &str, has: &str| -> String {
        use std::io::Write;
        let mut child = sb
            .cmd()
            .arg("hook-name")
            .env("ACA_SESSION_ID", session)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let payload = format!("{{\"hook_event_name\":\"{ev}\",\"session_id\":\"c1\",\"session_title\":\"{has}\"}}");
        child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success());
        String::from_utf8(o.stdout).unwrap()
    };
    let given = |out: &str, ev: &str| -> String {
        let v: Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("{e}: {out:?}"));
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], ev);
        v["hookSpecificOutput"]["sessionTitle"].as_str().unwrap().to_string()
    };
    assert_eq!(given(&ask(&id, "SessionStart", ""), "SessionStart"), "修登录");
    assert_eq!(ask(&id, "UserPromptSubmit", "修登录"), "");
    sb.aca(&["rename", &id, "修登录 2"]);
    assert_eq!(given(&ask(&id, "UserPromptSubmit", "修登录"), "UserPromptSubmit"), "修登录 2");
    assert_eq!(ask(&id, "UserPromptSubmit", "我在 Claude 里起的"), "");
    assert_eq!(ask(&id, "Stop", ""), "");
    assert_eq!(ask("", "UserPromptSubmit", ""), "");
}

/// The tmux status bar segment: how many need you and how many are finished and unread (no names), not counting the one you're
/// looking at, gone once seen; read-only, changes nothing else
#[test]
fn the_tmux_status_segment_counts_what_waits_for_you() {
    let Some(sb) = Sandbox::new("bar") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "修登录"]);
    let pane = sb.pane(&id);
    let hook = |ev: &str, more: &str| sb.hook(&id, &pane, &format!("{{\"hook_event_name\":\"{ev}\",\"session_id\":\"c1\"{more}}}"));
    assert_eq!(sb.aca(&["tmux-status"]), "");
    hook("UserPromptSubmit", ",\"prompt\":\"看看\"");
    assert_eq!(sb.aca(&["tmux-status"]), "");
    hook("PermissionRequest", ",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"ls\"}");
    assert_eq!(sb.aca(&["tmux-status"]), "#[fg=red reverse bold] ◆ 1 #[default]");
    // Looking at it right now: doesn't count
    assert_eq!(sb.aca(&["tmux-status", &id]), "");
    hook("Stop", ",\"last_assistant_message\":\"好了\",\"background_tasks\":[]");
    assert_eq!(sb.aca(&["tmux-status"]), "#[fg=brightblack]◐ 1#[default]");
    let events = sb.event_names().len();
    sb.aca(&["seen", &id]);
    assert_eq!(sb.aca(&["tmux-status"]), "");
    // Asking several times adds no events
    assert_eq!(sb.event_names().len(), events);
}

/// A plugin's event hook failed to run: `aca plugin ls` and the status bar segment both mark it (which plugin, which hook); once
/// a later turn runs fine, it's no longer mentioned.
/// A disabled plugin isn't mentioned, and is again once enabled; linking again (the manifest may have changed, so the hook
/// numbers may not match) clears the record
#[test]
fn a_failing_hook_is_flagged_until_it_runs_fine_again() {
    let Some(sb) = Sandbox::new("health") else { return };
    let dir = sb.home.join("flaky");
    write(
        &dir.join("aca-plugin.toml"),
        "id = \"test.flaky\"\nname = \"Flaky\"\nversion = \"0.1.0\"\n[[events]]\non = \"turn.done\"\ncommand = [\"sh\", \"flaky.sh\"]\n",
    );
    // Exits 3 when the state directory has a file named fail
    write(&dir.join("flaky.sh"), "[ -e \"$ACA_PLUGIN_STATE_DIR/fail\" ] && exit 3\nexit 0\n");
    sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    let fail = sb.home.join("state/plugins/test.flaky/fail");
    write(&fail, "");
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "写文档"]);
    let pane = sb.pane(&id);
    let rec = sb.home.join("state/plugin-health/test.flaky/hook-0");
    // One turn: say something, answer; hooks aren't waited for, so wait until it records its exit code
    let turn = |want: &str| {
        sb.hook(&id, &pane, "{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"c1\",\"prompt\":\"再来\"}");
        sb.hook(&id, &pane, "{\"hook_event_name\":\"Stop\",\"session_id\":\"c1\",\"last_assistant_message\":\"好\"}");
        assert!(
            sb.until(|| std::fs::read_to_string(&rec).is_ok_and(|t| t.trim() == want)),
            "钩子没跑完:{:?} {}",
            std::fs::read_to_string(&rec),
            sb.aca(&["plugin", "log", "test.flaky"])
        );
    };
    // The session you're looking at isn't counted among those that need you: only the plugin half of the segment remains
    let bar = || sb.aca(&["tmux-status", &id]);
    turn("3");
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("⚠ test.flaky:钩子 turn.done 退出码 3"), "{ls}");
    assert_eq!(bar(), "⚠ test.flaky");

    std::fs::remove_file(&fail).unwrap();
    turn("0");
    assert!(!sb.aca(&["plugin", "ls"]).contains("⚠ test.flaky"));
    assert_eq!(bar(), "");

    write(&fail, "");
    turn("3");
    sb.aca(&["plugin", "disable", "test.flaky"]);
    assert_eq!(bar(), "");
    sb.aca(&["plugin", "enable", "test.flaky"]);
    assert_eq!(bar(), "⚠ test.flaky");
    sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    assert_eq!(bar(), "");
}

/// aca tmux install / uninstall: aca's part is written in aca's own directory, your tmux.conf gains just two lines (before TPM),
/// and a running tmux uses it right away;
/// installing twice doesn't duplicate; after uninstalling, tmux.conf is back to the original character for character, and aca's
/// keys and status bar segment are taken out of the running tmux too
#[test]
fn aca_tmux_install_adds_two_lines_and_uninstall_puts_everything_back() {
    let Some(sb) = Sandbox::new("tmuxconf") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "x"]);
    let right = || sb.tmux(&["show-options", "-gv", "status-right"]);
    let ours_bound = || {
        let keys = sb.tmux(&["list-keys", "-T", "prefix"]) + &sb.tmux(&["list-keys", "-T", "root"]);
        keys.lines().filter(|l| l.contains("#{socket_path}") && l.contains(" ui --client")).count()
    };
    let before = right();
    // With ACA_HOME set, "your tmux.conf" is $ACA_HOME/tmux.conf
    let conf = sb.home.join("tmux.conf");
    let original = "set -g status-right \"MINE\"\nbind a run-shell 'tmux display-popup \"aca ui --client #{client_name}\"'\n\n# ====\n# TPM\n# ====\nrun '~/.tmux/plugins/tpm/tpm'\n";
    write(&conf, original);
    assert!(sb.aca(&["tmux"]).contains("未安装"));
    let out = sb.aca(&["tmux", "install"]);
    assert!(out.contains("第 5 行添加") && out.contains("运行中的 tmux 已生效") && out.contains("第 2 行是你自己添加的"), "{out}");
    let text = std::fs::read_to_string(&conf).unwrap();
    let ours = sb.home.join("config/tmux.conf");
    assert!(text.contains("\n# aca:") && text.contains(&format!("source-file -q {}\n\n# ====\n# TPM", ours.display())), "{text}");
    assert!(std::fs::read_to_string(&ours).unwrap().contains("tmux-status #{@aca_id}"));
    assert_eq!(ours_bound(), 2);
    assert_eq!(right().matches("tmux-status").count(), 1, "{}", right());
    // Installing again: that part is rewritten, tmux.conf gains no extra line, and the status bar segment isn't attached twice
    assert!(sb.aca(&["tmux", "install"]).contains("中已有 source 它的行"));
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), text);
    assert_eq!(right().matches("tmux-status").count(), 1);
    assert!(sb.aca(&["tmux"]).contains("已安装"));
    // A session that set its own status-right (the ones agent-deck made): the global one is still what's looked at, not attached
    // again
    sb.tmux(&["set-option", "-t", &sb.pane(&id), "status-right", "这个会话自己的"]);
    assert!(!sb.aca(&["tmux", "install"]).contains("已去重"));
    assert_eq!(right().matches("tmux-status").count(), 1, "{}", right());
    // Duplicates attached earlier: folded into one when installing, with your own parts unchanged
    let file = std::fs::read_to_string(&ours).unwrap();
    let seg = file.lines().find_map(|l| l.trim().strip_prefix("set -ag status-right \"")?.strip_suffix('"')).unwrap();
    let one = right();
    sb.tmux(&["set-option", "-ag", "status-right", &seg.repeat(3 - one.matches(seg).count())]);
    let out = sb.aca(&["tmux", "install"]);
    assert!(out.contains("原先重复了 3 次,已去重"), "{out}\n{}", right());
    assert_eq!(right().matches(seg).count(), 1, "{}", right());
    assert_eq!(right().replace(seg, ""), one.replace(seg, ""));
    // Uninstall: back to the original
    let out = sb.aca(&["tmux", "uninstall"]);
    assert!(out.contains("配置的两行") && out.contains("已同时移除"), "{out}");
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), original);
    assert!(!ours.exists());
    assert_eq!(ours_bound(), 0);
    assert_eq!(right(), before);
    assert!(sb.aca(&["tmux", "uninstall"]).contains("中没有 source aca 配置的行"));
}

/// aca upgraded without running install.sh (brew and the like): as soon as the UI opens, aca's tmux settings are rewritten for
/// this build, and a running tmux uses them too.
/// Left alone when that part was written by another aca (built in a worktree, installed some other way)
#[test]
fn opening_the_interface_brings_aca_s_tmux_settings_up_to_this_version() {
    let Some(sb) = Sandbox::new("tmuxfresh") else { return };
    sb.aca(&["new", "fake", &sb.proj(), "-t", "x"]);
    sb.aca(&["tmux", "install"]);
    let ours = sb.home.join("config/tmux.conf");
    let now = std::fs::read_to_string(&ours).unwrap();
    // Written by an "old build": no prefix + a key yet, and none in the running tmux either
    let old: String = now.lines().filter(|l| !l.starts_with("bind a ")).map(|l| format!("{l}\n")).collect();
    assert_ne!(old, now);
    sb.tmux(&["unbind-key", "-T", "prefix", "a"]);
    let bound = || sb.tmux(&["list-keys", "-T", "prefix"]).lines().any(|l| l.contains(" ui --client"));
    assert!(!bound());
    let script = sb.home.join("ui.sh");
    let q = |p: &Path| format!("'{}'", p.display());
    write(
        &script,
        &format!(
            "unset TMUX TMUX_PANE KITTY_WINDOW_ID TERM_PROGRAM SSH_CONNECTION SSH_TTY\n\
             export HOME={home} ACA_HOME={home} ACA_TMUX_SOCKET={sock} ACA_LANG=zh ACA_IME=0 TERM=xterm-256color\n\
             exec {aca} ui\n",
            home = q(&sb.home),
            sock = sb.socket,
            aca = q(Path::new(env!("CARGO_BIN_EXE_aca"))),
        ),
    );
    let open = |name: &str| {
        sb.tmux(&["new-session", "-d", "-s", name, "-x", "120", "-y", "30", "/bin/sh", script.to_str().unwrap()]);
        assert!(sb.until(|| sb.tmux(&["capture-pane", "-p", "-t", name]).contains("快捷键")), "界面没打开");
    };

    // Written by another aca: left alone
    let elsewhere = old.replace(env!("CARGO_BIN_EXE_aca"), "/elsewhere/aca");
    assert_ne!(elsewhere, old);
    write(&ours, &elsewhere);
    open("first");
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert_eq!(std::fs::read_to_string(&ours).unwrap(), elsewhere);
    assert!(!bound());

    // An old one written by this aca: rewritten for this build, and the running tmux gets the key too
    write(&ours, &old);
    open("second");
    assert!(sb.until(|| std::fs::read_to_string(&ours).unwrap() == now), "没重写");
    assert!(sb.until(bound), "开着的 tmux 没换上");
}

/// Claude's status bar (aca statusline): for a session that's idle with nobody attached and watching, print exactly what was
/// drawn last time (other sessions working change the quota, and redrawing along with them
/// would keep the screen producing output, so auto-stop would never see it go idle); talking ones and ones outside tmux are
/// recomputed as usual
#[test]
fn an_idle_unwatched_session_prints_its_last_status_bar() {
    use std::io::Write as _;
    let Some(sb) = Sandbox::new("bar") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "x"]);
    let pane = sb.pane(&id);
    let transcript = sb.home.join("t.jsonl");
    write(&transcript, "{}\n");
    let bar = |model: &str, in_tmux: bool| {
        let payload = serde_json::json!({
            "session_id": "0f6c1d2e-aaaa-bbbb-cccc-000000000001",
            "transcript_path": transcript,
            "model": {"display_name": model},
        });
        let mut c = sb.cmd();
        c.arg("statusline").env("TMPDIR", &sb.home).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
        if in_tmux {
            c.env("TMUX_PANE", &pane);
        }
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
        String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned()
    };
    // Just talked: drawn as usual
    assert!(bar("Opus", true).contains("Opus"));
    // Idle for a minute, nobody attached: even with the model changed, it's still the last one
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    std::fs::File::options().write(true).open(&transcript).unwrap().set_modified(old).unwrap();
    let kept = bar("Sonnet", true);
    assert!(kept.contains("Opus") && !kept.contains("Sonnet"), "{kept}");
    // Not in tmux (no way to ask whether anyone is watching): recomputed as usual
    assert!(bar("Sonnet", false).contains("Sonnet"));
}

/// One that can't start at all (the program isn't on PATH): the error says what's missing, the registration is withdrawn, and
/// the list keeps no stopped row for it
#[test]
fn a_session_whose_program_isnt_there_leaves_nothing_behind() {
    let Some(sb) = Sandbox::new("nolaunch") else { return };
    let cfg = sb.home.join("config/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap() + "\n[tools.gone]\nkind = \"claude\"\ncmd = [\"aca-no-such-agent\"]\n";
    std::fs::write(&cfg, text).unwrap();
    let o = sb.run(&["new", "gone", &sb.proj(), "-t", "ghost"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("PATH 中找不到 aca-no-such-agent"), "{err}");
    assert_eq!(sb.json(&["ls", "--json", "--no-screen"]), serde_json::json!([]));
    let ev = sb.event_names();
    assert!(ev.ends_with(&["session.created".to_string(), "session.removed".to_string()]), "{ev:?}");
}

/// Your own plugin: it exists once ~/.config/aca/user/ has a manifest (no link needed), and is gone once that's deleted; sync
/// doesn't delete it, remove can't delete it.
/// cwd = "session" runs in the session directory, and input hands it the session's last reply / screen (a pane reads
/// ACA_INPUT_FILE)
#[test]
fn your_own_plugin_is_there_without_linking_and_gets_the_sessions_text() {
    let Some(sb) = Sandbox::new("mine") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    let pane = sb.pane(&id);
    sb.wait_screen(&pane, "fake agent up");
    sb.hook(
        &id,
        &pane,
        r#"{"hook_event_name":"Stop","session_id":"c1","last_assistant_message":"看 https://x.dev/a","background_tasks":[]}"#,
    );
    let manifest = sb.home.join("config/user/aca-plugin.toml");
    write(
        &manifest,
        "[[commands]]\nid = \"where\"\ntitle = \"在哪\"\ncwd = \"session\"\ninput = \"reply\"\ncommand = [\"sh\", \"-c\", \"pwd; cat\"]\n\
         [[commands]]\nid = \"screen\"\ninput = \"screen\"\ncommand = [\"cat\"]\n\
         [[panes]]\nid = \"dump\"\ninput = \"reply\"\ncwd = \"session\"\ncommand = [\"./dump.sh\"]\n\
         [[keys]]\nkey = \"w\"\nrun = \"where\"\n",
    );
    write(&sb.home.join("config/user/dump.sh"), "#!/bin/sh\n{ pwd; cat \"$ACA_INPUT_FILE\"; } > \"$ACA_PLUGIN_STATE_DIR/pane.txt\"\n");
    let mut perm = std::fs::metadata(sb.home.join("config/user/dump.sh")).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(sb.home.join("config/user/dump.sh"), perm).unwrap();
    // Not linked: it's in the list anyway, marked as your own
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("● user") && ls.contains("你自己的"), "{ls}");
    assert!(sb.aca(&["plugin", "key"]).contains("w  user.where  在哪"));
    // Runs in the session directory, with the last reply on stdin
    let out = sb.aca(&["plugin", "run", "user.where", "--session", &id]);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].ends_with("/proj") && lines[1] == "看 https://x.dev/a", "{out}");
    // The text on screen
    assert!(sb.aca(&["plugin", "run", "user.screen", "--session", &id]).contains("fake agent up"));
    // A pane: opens in a new window of the session, with the content in ACA_INPUT_FILE; ./dump.sh is still looked up in the plugin
    // directory
    sb.aca(&["plugin", "pane", "user", "dump", "--session", &id, "--placement", "window"]);
    let dumped = sb.home.join("state/plugins/user/pane.txt");
    assert!(sb.until(|| std::fs::read_to_string(&dumped).is_ok_and(|t| t.contains("x.dev"))), "窗格没跑");
    let t = std::fs::read_to_string(&dumped).unwrap();
    assert!(t.lines().next().unwrap().ends_with("/proj"), "{t}");
    // sync doesn't delete it; remove can't, and says what to do
    sb.aca(&["plugin", "sync"]);
    assert!(sb.aca(&["plugin", "ls"]).contains("● user"));
    let o = sb.run(&["plugin", "remove", "user"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("是你自己的"), "{}", String::from_utf8_lossy(&o.stderr));
    // Disabled: recorded in plugins.toml, gone from the menu
    sb.aca(&["plugin", "disable", "user"]);
    assert!(sb.aca(&["plugin", "ls"]).contains("○ user"));
    assert!(!sb.aca(&["plugin", "key"]).contains("user.where"));
    sb.aca(&["plugin", "enable", "user"]);
    // Broken: the list says what's wrong
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(&manifest, "[[commands]]\nid = \"x\"\n");
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("user") && ls.contains("清单有误") && ls.contains("command"), "{ls}");
    // Deleted, it's gone
    std::fs::remove_file(&manifest).unwrap();
    assert!(!sb.aca(&["plugin", "ls"]).contains("user"));
}

#[test]
fn routers_get_only_their_own_tools_and_dispatch_literal_text() {
    use serde_json::json;
    let Some(sb) = Sandbox::new("router-tools") else { return };
    let dir = sb.proj();
    let scratch = sb.home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    add_config(&sb, &format!("[scratch]\ndir = {:?}\n", scratch));
    sb.aca(&["group", "create", "p", &dir]);
    sb.aca(&["group", "create", "else", scratch.to_str().unwrap()]);
    let new = |g: &str, title: &str| sb.aca(&["new", "fake", &dir, "-g", g, "-t", title, "--hold"]);
    let worker = new("p", "worker");
    let router = new("p", "router");
    let desk = new("else", "desk");
    sb.aca(&["plugin", "install", "aca.core"]);
    sb.aca(&["group", "router", "p", &router]);
    sb.aca(&["group", "router", "else", &desk]);
    sb.aca(&["start", &router]);
    let ask = |who: &str, method: &str, params: Value| {
        let msg = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let out = sb.as_session(who, &["plugin", "tools"], &format!("{msg}\n"));
        assert!(out.status.success(), "{out:?}");
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["result"].clone()
    };
    let names = |who: &str| {
        ask(who, "tools/list", json!({}))["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let mut want: Vec<String> = "sessions look tickets ticket start give set message move note notes wake_me"
        .split_whitespace()
        .map(|id| format!("aca-router__{id}"))
        .collect();
    assert_eq!(names(&router), want);
    want.push("aca-router__groups".into());
    assert_eq!(names(&desk), want);
    assert!(names(&worker).iter().all(|n| n.starts_with("aca-core__")));
    let call = |who: &str, tool: &str, args: Value, ok: bool| {
        let r = ask(who, "tools/call", json!({"name":format!("aca-router__{tool}"),"arguments":args}));
        assert_eq!(r["isError"], !ok, "{tool}: {r}");
        r["content"][0]["text"].as_str().unwrap().trim_end_matches('\n').to_owned()
    };
    let payload = "  | > $ ; \x60ticks\x60 \"double\" 'single'\n--help\n";
    let run = |tool: &str, args| call(&router, tool, args, true);
    let inbox = |id: &str| sb.json(&["inbox", id, "--json"]);
    assert!(call(&router, "sessions", json!({"group":"else"}), false).contains("p"));
    for id in ["groups", "rm", "stop"] {
        call(&router, id, json!({}), false);
    }
    assert!(call(&desk, "groups", json!({}), true).contains(&format!("p  {dir}  @{router}")));
    assert_eq!(run("ticket", json!({"title":"first","criteria":"done","to":worker,"say":payload})), "#1");
    let brief = format!("任务 #1:first\n完成标准:done\n{payload}\n做完后运行:aca task done 1 \"一句话说结果\"");
    assert_eq!(inbox(&worker)[0]["text"], brief);
    let sessions = run("sessions", json!({}));
    assert!(sessions.lines().any(|l| l.starts_with(&worker) && l.contains("worker  #1")), "{sessions}");
    for listing in [sessions, call(&desk, "sessions", json!({"group":"p"}), true)] {
        assert!(listing.lines().any(|l| l.starts_with(&router) && l.ends_with("  router  router")), "{listing}");
        assert!(listing.lines().any(|l| l.starts_with(&worker) && l.ends_with("  #1")), "{listing}");
    }
    let tail = "答".repeat(2000);
    sb.hook(&worker, "", &json!({"hook_event_name":"Stop","session_id":"w","last_assistant_message":format!("skip{tail}")}).to_string());
    let look = run("look", json!({"id":worker}));
    assert!(look.lines().next().unwrap().contains("worker"), "{look}");
    assert_eq!(look.lines().nth(1), Some(tail.as_str()));
    assert!(run("tickets", json!({})).contains("#1  ● first"));
    assert!(run("give", json!({"ticket":1,"to":"-"})).contains("#1  · first"));
    assert!(run("give", json!({"ticket":1,"to":worker,"say":payload})).contains("#1  ● first"));
    assert_eq!(inbox(&worker).as_array().unwrap().last().unwrap()["text"], brief);
    assert!(run("set", json!({"ticket":1,"note":"wait","title":"renamed","criteria":"ready"})).contains("renamed"));
    assert_eq!(sb.json(&["task", "show", "1", "--json"])["note"], "wait");
    assert!(run("message", json!({"to":worker,"text":payload})).contains(&worker));
    assert_eq!(inbox(&worker).as_array().unwrap().last().unwrap()["text"], payload);
    let note = run("note", json!({"text":"remember\nsecond line"}));
    assert!(run("notes", json!({})).contains(&note));
    assert_eq!(sb.json(&["note", "show", &note, "--json"])["text"], "remember\nsecond line");
    assert!(run("wake_me", json!({"after":"1h","text":payload})).contains(&router));
    assert_eq!(inbox(&router).as_array().unwrap().last().unwrap()["text"], payload);
    let other = new("else", "other");
    run("move", json!({"ids":other}));
    assert_eq!(sb.json(&["show", &other, "--json"])["group"], "p");
    call(&router, "start", json!({"title":"missing","say":"x","dir":"missing"}), false);
    assert!(!Path::new(&dir).join("missing").exists());
    write(&sb.home.join("agent.sh"), "#!/bin/sh\nprintf '%s' \"$@\" > \"$ACA_HOME/open-$ACA_SESSION_ID\"\nexec sleep 600\n");
    let text = run("start", json!({"title":"second","say":payload,"criteria":"ready"}));
    let id = text.strip_prefix("#2 ").unwrap();
    let s = sb.json(&["show", id, "--json"]);
    assert_eq!((&s["group"], &s["parent"], &s["tool"]), (&json!("p"), &json!(""), &json!("fake")));
    assert!(!s["pane"].as_str().unwrap().is_empty());
    assert_eq!(sb.json(&["task", "show", "2", "--json"])["session"], id);
    let opening = format!("任务 #2:second\n完成标准:ready\n{payload}\n做完后运行:aca task done 2 \"一句话说结果\"");
    assert!(sb.until(|| std::fs::read_to_string(sb.home.join(format!("open-{id}"))).is_ok_and(|s| s.ends_with(&opening))));
    let sub = Path::new(&dir).join("sub");
    std::fs::create_dir(&sub).unwrap();
    let n = run("ticket", json!({"title":"third","criteria":"ok"}));
    assert_eq!(n, "#3");
    let start = run("start", json!({"ticket":3,"title":"next","say":"next","dir":"sub","tool":"fake"}));
    let next = start.strip_prefix("#3 ").unwrap();
    assert_eq!(sb.json(&["show",next,"--json"])["dir"],sub.canonicalize().unwrap().to_string_lossy().as_ref());
    assert_eq!(sb.json(&["task","show","3","--json"])["session"],next);
    sb.aca(&["task", "done", "1", "finished"]);
    assert!(run("tickets", json!({})).lines().any(|l| l.contains("✓ renamed") && l.ends_with("finished")));

    sb.aca(&["plugin", "install", "aca.gateway"]);
    for (who, expected) in [(&desk, want.as_slice()), (&router, &want[..12])] {
        let mut c = GwConn::open(&sb, who);
        assert_eq!(c.tools(), expected);
        assert!(c.call(3, "aca-router__notes").contains("remember") || who == &desk);
        c.close();
    }
    let mut c = GwConn::open(&sb, &worker);
    assert_eq!(c.tools(), names(&worker));
    c.close();
    let mut c = GwConn::open(&sb, "");
    assert!(c.tools().iter().all(|n| !n.starts_with("aca-router__")));
    assert_eq!(c.ask(3, "tools/call", json!({"name":"aca-router__notes","arguments":{}}))["result"]["isError"], true);
    c.close();
    sb.aca(&["plugin", "gateway", "stop"]);
    sb.aca(&["stop", &router]);
    sb.aca(&["plugin", "disable", "aca.router"]);
    let out = sb.run(&["start", &router]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("aca.router 已关闭"), "{out:?}");
}

#[test]
fn installing_router_tools_preserves_an_ordinary_launch_and_its_messages_byte_for_byte() {
    let Some(sb) = Sandbox::new("router-ordinary") else { return };
    write(&sb.home.join("agent.sh"), "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$ACA_HOME/argv-$ACA_SESSION_ID\"\nexec sleep 600\n");
    let id = sb.aca(&["new", "fake", &sb.proj(), "-g", "p"]);
    let record = sb.home.join(format!("argv-{id}"));
    assert!(sb.until(|| record.exists()));
    let before = std::fs::read(&record).unwrap();
    let text = "| > $ ; \"quoted\"\nsecond line";
    sb.aca(&["msg", &id, text]);
    assert_eq!(sb.json(&["inbox", &id, "--json"])[0]["text"], text);
    sb.aca(&["stop", &id]);
    std::fs::remove_file(&record).unwrap();
    sb.aca(&["plugin", "install", "aca.router"]);
    sb.aca(&["start", &id]);
    assert!(sb.until(|| std::fs::read(&record).is_ok_and(|v| v == before)));
    sb.aca(&["msg", &id, text]);
    assert_eq!(sb.json(&["inbox", &id, "--json"]).as_array().unwrap().last().unwrap()["text"], text);
}

#[test]
fn a_codex_router_keeps_its_existing_ssh_command() {
    use std::os::unix::fs::PermissionsExt;
    let Some(sb) = Sandbox::new("router-ssh-codex") else { return };
    let ssh = sb.home.join("bin/ssh");
    let saved = sb.home.join("ssh-command");
    // Record the remote command without connecting anywhere or executing it.
    write(
        &ssh,
        &format!(
            "#!/bin/sh\nfor arg do last=$arg; done\ncase \"$last\" in *\"'codex'\"*) printf %s \"$last\" > {:?}; exec sleep 600;; esac\n",
            saved
        ),
    );
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dir = sb.proj();
    add_config(
        &sb,
        &format!(
            "[sandbox.remote]\nssh='fake'\nworkspace={:?}\ndirs=[{dir:?}]\n[accounts.codex]\nx={{sandbox='remote',args=['--account-arg']}}\n",
            sb.home.join("workspace")
        ),
    );
    let path = format!("{}:{}", ssh.parent().unwrap().display(), std::env::var("PATH").unwrap());
    let run = |args: &[&str]| {
        let out = sb.cmd().env("PATH", &path).args(args).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let id = run(&["new", "codex:x", &dir, "-g", "g", "--", "--saved-arg"]);
    assert!(sb.until(|| saved.exists()));
    let ordinary = std::fs::read_to_string(&saved).unwrap();
    assert!(ordinary.ends_with("'codex' '--account-arg' '--saved-arg'"), "{ordinary}");
    run(&["stop", &id]);
    run(&["group", "router", "g", &id]);
    std::fs::remove_file(&saved).unwrap();
    run(&["start", &id]);
    assert!(sb.until(|| saved.exists()));
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), ordinary);
}

#[test]
fn claude_router_launch_uses_the_selected_account_and_preserves_ordinary_sessions() {
    let Some(sb) = Sandbox::new("router-account") else { return };
    let cfg = sb.home.join("config/config.toml");
    let ws = sb.home.join("workspace");
    let dir = sb.proj();
    write(
        &sb.home.join("agent.sh"),
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$ACA_HOME/args-$ACA_SESSION_ID\"\nprintf %s \"$ROUTER_ACCOUNT_ENV\" > \"$ACA_HOME/env-$ACA_SESSION_ID\"\nif [ -n \"$ACA_ROUTER_SHIFT\" ]; then exec python3 \"$ACA_HOME/router.py\" \"$@\"; fi\nexec sleep 600\n",
    );
    add_config(
        &sb,
        &format!(
            "[tools.probe]\nkind='claude'\ncmd=['/bin/sh',{:?}]\nargs=['--tool-only']\n\
        [sandbox.here]\nworkspace={ws:?}\nenv={{ROUTER_ACCOUNT_ENV='local-value'}}\n\
        [sandbox.remote]\nworkspace={ws:?}\nssh='unreachable'\n\
        [accounts.claude]\nhere={{sandbox='here',args=['--account-only']}}\nremote={{sandbox='remote'}}\n",
            sb.home.join("agent.sh")
        ),
    );
    let ordinary = sb.aca(&["new", "probe:here", &dir, "-g", "workers", "--", "--saved"]);
    let args_path = |id: &str| sb.home.join(format!("args-{id}"));
    let read = |id: &str| std::fs::read(args_path(id)).unwrap();
    assert!(sb.until(|| args_path(&ordinary).is_file()));
    let before = read(&ordinary);
    let received = "hello | ' $ \n世界";
    sb.aca(&["msg", &ordinary, received]);
    sb.aca(&["stop", &ordinary]);
    std::fs::remove_file(args_path(&ordinary)).unwrap();
    let r = sb.aca(&["new", "probe:here", &dir, "-g", "p", "--hold", "--", "--old-session"]);
    sb.aca(&["group", "router", "p", &r]);
    let old = std::fs::read_to_string(&cfg).unwrap();
    write(&cfg, &format!("{old}\n[router]\nmodel='opus'\neffort='medium'\n"));
    sb.aca(&["start", &r]);
    assert!(sb.until(|| sb.home.join(format!("env-{r}")).is_file()));
    assert_eq!(std::fs::read_to_string(sb.home.join(format!("env-{r}"))).unwrap(), "local-value");
    let bytes = read(&r);
    let text = std::str::from_utf8(&bytes).unwrap();
    let args: Vec<_> = text.trim_end_matches('\0').split('\0').collect();
    let value = |key| args[args.iter().position(|a| *a == key).unwrap() + 1];
    assert_eq!(
        (value("--agent"), value("--model"), value("--effort"), value("--tools")),
        ("aca-router:router", "opus", "medium", "AskUserQuestion")
    );
    assert!(args.contains(&"--account-only") && !args.contains(&"--tool-only") && !args.contains(&"--old-session"));
    let read_json = |path: &str| serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap();
    let settings = read_json(value("--settings"));
    assert_eq!(settings["autoMemoryEnabled"], false);
    assert!(settings["hooks"]["SessionStart"].is_array());
    assert_eq!(read_json(value("--mcp-config"))["mcpServers"].as_object().unwrap().keys().collect::<Vec<_>>(), ["aca"]);
    let plugins: Vec<_> = args.windows(2).filter_map(|w| (w[0] == "--plugin-dir").then_some(w[1])).collect();
    assert_eq!(plugins.len(), 2);
    assert!(Path::new(plugins[0]).join("hooks/register.js").is_file());
    for role in ["router", "desk", "router-en", "desk-en"] {
        assert!(Path::new(plugins[1]).join(format!("agents/{role}.md")).is_file());
    }
    assert!(!text.contains("项目交接"));
    let opening = shift_prompt(&sb, sb.json(&["show", &r, "--json"])["conv"].as_str().unwrap());
    assert!(opening.contains("项目交接 · p"));
    assert!(opening.ends_with("\n\n看完等用户说话,只回一个字。"));
    sb.aca(&["start", &ordinary]);
    assert!(sb.until(|| args_path(&ordinary).is_file()));
    assert_eq!(read(&ordinary), before);
    sb.aca(&["msg", &ordinary, received]);
    let inbox = sb.json(&["inbox", &ordinary, "--json"]);
    for i in [0, 1] {
        assert_eq!(inbox[i]["text"], received);
    }

    // A change in the registered account binding is rechecked before restarting the router.
    sb.aca(&["stop", &r]);
    write(&cfg, &std::fs::read_to_string(&cfg).unwrap().replace("here={sandbox='here'", "here={sandbox='remote'"));
    std::fs::remove_file(args_path(&r)).unwrap();
    let refused = sb.run(&["start", &r]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("SSH 沙箱无法使用 router 工具"));
    assert!(!args_path(&r).exists());
}

/// The tool names the plugin capability server lists for agents (`aca plugin tools`, no session)
fn agent_tools(sb: &Sandbox, call: Option<&str>) -> (Vec<String>, String) {
    use std::io::Write;
    let mut child =
        sb.cmd().args(["plugin", "tools"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut req = vec![
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-11-25" } }),
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    ];
    if let Some(name) = call {
        req.push(serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": name, "arguments": {} } }));
    }
    let mut stdin = child.stdin.take().unwrap();
    for r in req {
        writeln!(stdin, "{r}").unwrap();
    }
    drop(stdin);
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    let mut names = vec![];
    let mut said = String::new();
    for v in out.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        match v["id"].as_u64() {
            Some(2) => names = v["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect(),
            Some(3) => said = v["result"]["content"][0]["text"].as_str().unwrap_or("").to_string(),
            _ => {}
        }
    }
    (names, said)
}

/// You decide: capabilities written as agent? go to agents only once you name them; you can change or remove keys in the list's ?,
/// and when two plugins want one key, it's named
#[test]
fn agent_tools_and_menu_keys_are_yours_to_decide() {
    let Some(sb) = Sandbox::new("yours") else { return };
    let perm = sb.home.join("perm");
    write(
        &perm.join("aca-plugin.toml"),
        "id = \"t.perm\"\nname = \"P\"\nversion = \"1\"\n\
         [[commands]]\nid = \"ls\"\ntitle = \"列\"\nexpose = [\"menu\", \"agent\"]\ncommand = [\"echo\", \"ls\"]\n\
         [[commands]]\nid = \"add\"\ntitle = \"加\"\nexpose = [\"menu\", \"agent?\"]\ncommand = [\"echo\", \"add\"]\n\
         [[keys]]\nkey = \"m\"\nrun = \"ls\"\n",
    );
    let other = sb.home.join("other");
    write(
        &other.join("aca-plugin.toml"),
        "id = \"t.other\"\nname = \"O\"\nversion = \"1\"\n[[commands]]\nid = \"x\"\ntitle = \"叉\"\ncommand = [\"true\"]\n[[keys]]\nkey = \"m\"\nrun = \"x\"\n",
    );
    sb.aca(&["plugin", "link", &perm.to_string_lossy()]);
    let out = sb.aca(&["plugin", "link", &other.to_string_lossy()]);
    assert!(out.contains("t.other"), "{out}");
    // agent? ones aren't given by default
    let (names, _) = agent_tools(&sb, None);
    assert_eq!(names, ["t-perm__ls"]);
    let choices = sb.aca(&["plugin", "allow", "t.perm"]);
    assert!(choices.contains("✓ ls") && choices.contains("○ add  需明确允许"), "{choices}");
    // Name one to give it; take back ls: written into plugins.toml, and the capability server lists and calls accordingly
    sb.aca(&["plugin", "allow", "t.perm", "add"]);
    assert_eq!(agent_tools(&sb, None).0, ["t-perm__ls", "t-perm__add"]);
    let out = sb.aca(&["plugin", "deny", "t.perm", "ls"]);
    assert!(out.contains("○ ls") && out.contains("✓ add") && out.contains("重启"), "{out}");
    assert!(std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("tools = [\"add\"]"));
    let (names, said) = agent_tools(&sb, Some("t-perm__ls"));
    assert_eq!(names, ["t-perm__add"]);
    assert!(said.contains("没有工具 t-perm__ls"), "{said}");
    assert_eq!(agent_tools(&sb, Some("t-perm__add")).1.trim(), "add");
    // What the manifest doesn't offer to agents can't be given; --reset goes back to the manifest's
    let o = sb.run(&["plugin", "allow", "t.other", "x"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("不能提供给 agent"));
    sb.aca(&["plugin", "allow", "t.perm", "--reset"]);
    assert_eq!(agent_tools(&sb, None).0, ["t-perm__ls"]);
    assert!(!std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("tools ="));

    // Both plugins' manifests want m: the one registered first gets it, the other is named (listing plugins says so too)
    let keys = sb.aca(&["plugin", "key"]);
    assert!(keys.contains("m  t.perm.ls  列") && keys.contains("⚠ 键 m 已分配给 t.perm「列」,t.other「叉」未分配到"), "{keys}");
    assert!(sb.aca(&["plugin", "ls"]).contains("⚠ 键 m 已分配给"));
    // Give t.other another one; remove t.perm's; then give m to t.other
    assert!(sb.aca(&["plugin", "key", "t.other", "x", "M"]).contains("按 M"));
    assert!(!sb.aca(&["plugin", "key"]).contains("⚠"));
    assert!(sb.aca(&["plugin", "key", "t.perm", "ls", "--none"]).contains("未设键"));
    sb.aca(&["plugin", "key", "t.other", "x", "m"]);
    let keys = sb.aca(&["plugin", "key"]);
    assert!(keys.contains("m  t.other.x") && !keys.contains("t.perm.ls"), "{keys}");
    // What you wrote wins: t.perm, back to its manifest's m, doesn't get it either
    sb.aca(&["plugin", "key", "t.perm", "ls", "--reset"]);
    let keys = sb.aca(&["plugin", "key"]);
    assert!(keys.contains("m  t.other.x") && keys.contains("⚠ 键 m 已分配给 t.other「叉」,t.perm「列」未分配到"), "{keys}");
    // An unusable key, a target that doesn't exist
    let o = sb.run(&["plugin", "key", "t.perm", "ls", "/"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("已被 ? 占用"));
    let o = sb.run(&["plugin", "key", "t.perm", "nope", "z"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("没有 nope"));
}

/// Hotkeys: while a plugin is enabled they're bound in tmux (the same way as Ctrl+Q, intercepted only in aca's sessions), and
/// changing the key, disabling, deleting follow along;
/// keys you bound yourself are left alone; after a tmux restart, they're restored when the next session is created
#[test]
fn a_plugins_hotkey_is_bound_in_tmux_while_the_plugin_is_on_and_yours_are_left_alone() {
    let Some(sb) = Sandbox::new("hotkey") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    let pane = sb.pane(&id);
    sb.wait_screen(&pane, "fake agent up");
    sb.hook(&id, &pane, r#"{"hook_event_name":"Stop","session_id":"c1","last_assistant_message":"跑 `make test`","background_tasks":[]}"#);
    let dir = sb.home.join("work/hot");
    write(
        &dir.join("aca-plugin.toml"),
        "id = \"t.hot\"\nname = \"Hot\"\nversion = \"1\"\n\
         [[panes]]\nid = \"pad\"\ntitle = \"便签\"\ninput = \"reply\"\ncommand = [\"sh\", \"-c\", \"cat \\\"$ACA_INPUT_FILE\\\" > \\\"$ACA_PLUGIN_STATE_DIR/got.txt\\\"\"]\n\
         [[panes]]\nid = \"other\"\ncommand = [\"true\"]\n\
         [[hotkeys]]\nkey = \"C-M-y\"\nrun = \"pad\"\n",
    );
    // The hotkeys aca bound for plugins in the key tables: "TABLE:KEY" (aca's own key — go to the next one that needs you — doesn't
    // count, it has its own test)
    let bound = || {
        let mut v: Vec<String> = sb
            .tmux(&["list-keys"])
            .lines()
            .filter(|l| l.contains("--hotkey") && l.contains(" plugin pane "))
            .filter_map(|l| {
                let mut w = l.split_once("-T")?.1.split_whitespace();
                // tmux 3.3a / 3.4 print C-M-y as M-C-y
                Some(format!("{}:{}", w.next()?, w.next()?.replace("M-C-", "C-M-")))
            })
            .collect();
        v.sort();
        v
    };
    let all = |k: &str| vec![format!("copy-mode-vi:{k}"), format!("copy-mode:{k}"), format!("root:{k}")];
    assert!(bound().is_empty());
    // Bound once linked: the normal key table, and the two for scrolling back
    sb.aca(&["plugin", "link", dir.to_str().unwrap()]);
    assert_eq!(bound(), all("C-M-y"));
    let root = sb.tmux(&["list-keys", "-T", "root"]);
    let line = root.lines().find(|l| l.contains("--hotkey") && l.contains(" plugin pane ")).unwrap();
    // Intercepted only in aca's sessions, the key is sent through in other sessions; pressing it opens this plugin's pane, aimed at
    // the session and terminal it was pressed in
    assert!(line.contains("#{?@aca_id,1,0}") && line.contains("send-keys C-M-y"), "{line}");
    assert!(line.contains("plugin pane t.hot pad --hotkey C-M-y --session '#{@aca_id}' --client '#{client_name}'"), "{line}");
    assert_eq!(sb.aca(&["plugin", "hotkey"]), "C-M-y  t.hot.pad  便签");
    // That command really runs: the pane gets the session's last reply
    sb.aca(&["plugin", "pane", "t.hot", "pad", "--hotkey", "C-M-y", "--session", &id, "--placement", "window"]);
    let got = sb.home.join("state/plugins/t.hot/got.txt");
    assert!(sb.until(|| std::fs::read_to_string(&got).is_ok_and(|t| t.contains("make test"))), "窗格没跑");
    // Change the key: the old one is unbound in all three places; written into plugins.toml. A pane the manifest gave no hotkey can
    // get one too; a second one can't take the same key, and is named
    assert!(sb.aca(&["plugin", "hotkey", "t.hot", "pad", "C-^"]).contains("在会话中按 C-^"));
    assert_eq!(bound(), all("C-^"));
    let decl = std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap();
    assert!(decl.contains("hotkeys = { pad = \"C-^\" }"), "{decl}");
    let out = sb.aca(&["plugin", "hotkey", "t.hot", "other", "C-^"]);
    assert!(out.contains("未设热键") && out.contains("键 C-^ 已分配给 t.hot「便签」"), "{out}");
    assert!(sb.aca(&["plugin", "ls"]).contains("键 C-^ 已分配给 t.hot「便签」"));
    sb.aca(&["plugin", "hotkey", "t.hot", "other", "--reset"]);
    // Bad ones aren't accepted: a single bare character, aca's own, not a pane
    for (pane, key, why) in [("pad", "y", "tmux"), ("pad", "C-q", "C-q"), ("pad", "C-M-c", "C-M-c"), ("nope", "M-y", "没有窗格 nope")] {
        let o = sb.run(&["plugin", "hotkey", "t.hot", pane, key]);
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains(why), "{pane} {key}:{o:?}");
    }
    assert_eq!(bound(), all("C-^"));
    // You bound that key to something else in tmux yourself: yours is left alone and named; in the two scrolling tables you didn't
    // bind it, so it's bound there
    sb.tmux(&["bind-key", "-n", "F5", "display-message", "mine"]);
    let out = sb.aca(&["plugin", "hotkey", "t.hot", "pad", "F5"]);
    assert!(out.contains("键 F5 已在 tmux 中绑定了其他操作,未修改"), "{out}");
    assert!(sb.tmux(&["list-keys", "-T", "root"]).lines().any(|l| l.contains("F5") && l.contains("display-message mine")));
    assert_eq!(bound(), ["copy-mode-vi:F5", "copy-mode:F5"]);
    // Back to the manifest's; no key; disable / enable again
    sb.aca(&["plugin", "hotkey", "t.hot", "pad", "--reset"]);
    assert_eq!(bound(), all("C-M-y"));
    assert!(sb.aca(&["plugin", "hotkey", "t.hot", "pad", "--none"]).contains("未设热键"));
    assert!(bound().is_empty());
    sb.aca(&["plugin", "hotkey", "t.hot", "pad", "--reset"]);
    sb.aca(&["plugin", "disable", "t.hot"]);
    assert!(bound().is_empty());
    // While disabled, a key left over from before is pressed (bound by hand here, pretending it wasn't unbound): no error, it's
    // unbound along the way
    sb.tmux(&["bind-key", "-n", "C-^", "run-shell", "aca plugin pane t.hot pad --hotkey C-^"]);
    let o = sb.run(&["plugin", "pane", "t.hot", "pad", "--hotkey", "C-^", "--session", &id]);
    assert!(o.status.success() && o.stdout.is_empty() && o.stderr.is_empty(), "{o:?}");
    assert!(bound().is_empty());
    sb.aca(&["plugin", "enable", "t.hot"]);
    assert_eq!(bound(), all("C-M-y"));
    // The tmux server restarts (the bindings aren't in tmux.conf): creating another session restores them
    sb.tmux(&["kill-server"]);
    sb.aca(&["new", "fake", &sb.proj(), "-t", "第二个"]);
    assert_eq!(bound(), all("C-M-y"));
    // No longer used: the key goes with it
    sb.aca(&["plugin", "unlink", "t.hot"]);
    assert!(bound().is_empty());
    // Your own one (no link needed): a hotkey written in its manifest is bound the next time aca is used; once deleted, it's unbound
    let mine = sb.home.join("config/user/aca-plugin.toml");
    write(&mine, "[[panes]]\nid = \"lg\"\ncommand = [\"true\"]\n[[hotkeys]]\nkey = \"M-=\"\nrun = \"lg\"\n");
    assert_eq!(sb.aca(&["plugin", "hotkey"]), "M-=  user.lg  lg");
    assert_eq!(bound(), all("M-="));
    std::fs::remove_file(&mine).unwrap();
    sb.aca(&["plugin", "ls"]);
    assert!(bound().is_empty());
}

/// The status bar shows only how many need you (`!` asking, `●` finished and unread); the key aca binds (`[keys] next` in
/// config.toml, Ctrl+Alt+N by default),
/// really pressed inside a session, takes this client to the next one: asking ones first, then finished-and-unread ones; once
/// all are seen, it cycles among the asking ones; with none at all, it says so.
/// The key can be changed or removed, and one you bound to something else yourself is left alone
#[test]
fn the_next_key_takes_the_client_to_what_waits_and_the_status_bar_only_counts() {
    let Some(sb) = Sandbox::new("next") else { return };
    let new = |t: &str| {
        let id = sb.aca(&["new", "fake", &sb.proj(), "-t", t]);
        let pane = sb.pane(&id);
        sb.wait_screen(&pane, "fake agent up");
        (id, pane)
    };
    let (here, here_pane) = new("我在这");
    let (done, done_pane) = new("跑完了");
    let (ask, ask_pane) = new("在问");
    let hook = |id: &str, pane: &str, ev: &str, more: &str| {
        sb.hook(id, pane, &format!("{{\"hook_event_name\":\"{ev}\",\"session_id\":\"c-{id}\"{more}}}"))
    };
    hook(&done, &done_pane, "UserPromptSubmit", ",\"prompt\":\"跑\"");
    hook(&done, &done_pane, "Stop", ",\"last_assistant_message\":\"好\"");
    hook(&ask, &ask_pane, "UserPromptSubmit", ",\"prompt\":\"跑\"");
    hook(&ask, &ask_pane, "PermissionRequest", ",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"ls\"}");
    // The status bar: kinds and counts, asking ones as a red block, finished ones in the minor text color; no names. The one you're
    // looking at doesn't count
    let bar = |id: &str| sb.aca(&["tmux-status", id]);
    assert_eq!(bar(&here), "#[fg=red reverse bold] ◆ 1 #[default] #[fg=brightblack]◐ 1#[default]");
    assert_eq!(bar(&ask), "#[fg=brightblack]◐ 1#[default]");

    // The key: bound when the session is created, in three key tables; intercepted only in aca's sessions, sent through as is
    // elsewhere
    let keys = |k: &str| sb.tmux(&["list-keys"]).lines().filter(|l| l.contains(" next --hotkey ") && l.contains(&format!(" {k} "))).count();
    assert_eq!(keys("C-M-n"), 3);
    let root = sb.tmux(&["list-keys", "-T", "root"]);
    let line = root.lines().find(|l| l.contains(" next --hotkey ")).unwrap();
    assert!(line.contains("#{?@aca_id,1,0}") && line.contains("send-keys C-M-n"), "{line}");
    assert!(line.contains("next --hotkey C-M-n --session '#{@aca_id}' --client '#{client_name}'"), "{line}");

    // A real client: attached from a pane of another tmux server, with keys pressed into that pane
    let outer = format!("{}-outer", sb.socket);
    let out = |args: &[&str]| {
        let mut c = Command::new("tmux");
        let o = scrub(&mut c).args(["-L", &outer, "-f", "/dev/null"]).args(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let attach = format!("tmux -L {} attach-session -t {here_pane}", sb.socket);
    out(&["new-session", "-d", "-x", "160", "-y", "40", &attach]);
    let at = || sb.tmux(&["list-clients", "-F", "#{@aca_id}"]);
    assert!(sb.until(|| at() == here), "客户端没连上:{}", at());
    let press = |key: &str| drop(out(&["send-keys", key]));
    // Asking ones first
    press("C-M-n");
    assert!(sb.until(|| at() == ask), "没进在问的那个:{}", at());
    // Press again: the finished-and-unread one. Having gone in counts as seen
    press("C-M-n");
    assert!(sb.until(|| at() == done), "没进跑完的那个:{}", at());
    assert!(sb.until(|| bar(&here) == "#[fg=red reverse bold] ◆ 1 #[default]"), "{}", bar(&here));
    // Press again: no unread ones left, one is still asking, back to it; pressed on it again, there are none at all, so it stays put
    // and says so
    press("C-M-n");
    assert!(sb.until(|| at() == ask), "没回到在问的那个:{}", at());
    press("C-M-n");
    assert!(sb.until(|| sb.tmux(&["show-messages"]).contains("暂无待处理会话")), "{}", sb.tmux(&["show-messages"]));
    assert_eq!(at(), ask);
    out(&["kill-server"]);

    // Typing it yourself works too (not in tmux, no terminal: can't attach, says it can't get in; doesn't count as seen)
    let o = sb.run(&["next", "--session", &ask]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("暂无待处理会话"), "{o:?}");

    // Change the key: the old one unbound, the new one bound, recorded in config.toml; one that can't serve as a hotkey isn't accepted
    let out = sb.aca(&["tmux", "key", "next", "M-n"]);
    assert!(out.contains("在会话中按 Alt+N(M-n)"), "{out}");
    assert_eq!((keys("C-M-n"), keys("M-n")), (0, 3));
    assert!(std::fs::read_to_string(sb.home.join("config/config.toml")).unwrap().contains("[keys]\nnext = \"M-n\""));
    let o = sb.run(&["tmux", "key", "next", "n"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("热键应写为带 Ctrl / Alt 的 tmux 键名"), "{o:?}");
    assert_eq!(keys("M-n"), 3);
    // You bound that key to something else in tmux yourself: yours is left alone and named
    sb.tmux(&["bind-key", "-n", "F7", "display-message", "mine"]);
    let out = sb.aca(&["tmux", "key", "next", "F7"]);
    assert!(out.contains("键 F7 已在 tmux 中绑定了其他操作,未修改"), "{out}");
    assert!(sb.tmux(&["list-keys", "-T", "root"]).lines().any(|l| l.contains(" F7 ") && l.contains("display-message mine")));
    // No key: all unbound, aca tmux says it's unbound; aca next can still be typed
    let out = sb.aca(&["tmux", "key", "next", "--none"]);
    assert!(out.contains("未绑定按键"), "{out}");
    assert_eq!(sb.tmux(&["list-keys"]).lines().filter(|l| l.contains(" next --hotkey ")).count(), 0);
    assert!(sb.aca(&["tmux"]).contains("未绑定按键"));
}

/// Ones that need you as seen only on screen (no hook reported it: Codex without trusted hooks, the confirmation box at start):
/// ◐ in the list, and the next key must be able to go there too
#[test]
fn the_next_key_also_goes_to_a_session_only_its_screen_says_is_waiting() {
    let Some(sb) = Sandbox::new("next-screen") else { return };
    sb.screen_tools();
    let here = sb.aca(&["new", "fake", &sb.proj(), "-t", "我在这"]);
    let w = sb.aca(&["new", "fcx", &sb.proj(), "-t", "门口"]);
    let pane = sb.pane(&w);
    sb.set_screen(
        &w,
        &pane,
        "Folder access\n  /p/api\n  Trust this folder? Codex can read, edit, and run files here.\n› 1. Trust and continue\n  2. Quit\n  enter continue · esc quit",
    );
    // Not in tmux, no terminal: it's the one picked, it just can't be attached to
    let o = sb.run(&["next", "--session", &here]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("无法进入「门口」"), "{o:?}");
}

/// The key aca binds for putting a session's reference where you type (`[keys] ref`, the prefix key then b by default, `aca ui --ref`),
/// really pressed inside a session, pops the list up on that client; ⏎ on a session pastes its reference (the two lines `B`
/// copies) into the pane the key was pressed in, without Enter, and the list closes. The key changes and goes away like the
/// next key
#[test]
fn the_ref_key_pops_up_the_list_and_puts_the_picked_reference_where_you_were_typing() {
    let Some(sb) = Sandbox::new("ref") else { return };
    let new = |t: &str| {
        let id = sb.aca(&["new", "fake", &sb.proj(), "-t", t]);
        let pane = sb.pane(&id);
        sb.wait_screen(&pane, "fake agent up");
        (id, pane)
    };
    let (here, here_pane) = new("我在这");
    let (there, _) = new("对面");

    // Bound when the session is created, beside the next key: in the prefix table alone, nothing passed on elsewhere
    let keys =
        |k: &str| sb.tmux(&["list-keys"]).lines().filter(|l| l.contains(" ui --ref --hotkey ") && l.contains(&format!(" {k} "))).count();
    assert_eq!(keys("b"), 1);
    let prefix = sb.tmux(&["list-keys", "-T", "prefix"]);
    let line = prefix.lines().find(|l| l.contains(" ui --ref --hotkey ")).unwrap();
    assert!(line.contains("#{?@aca_id,1,0}") && !line.contains("send-keys"), "{line}");
    assert!(line.contains("ui --ref --hotkey 'prefix b' --client '#{client_name}' --pane '#{pane_id}'"), "{line}");

    // A real client (a pane of another tmux server) on this session; the popup is drawn on it, so it shows in that pane
    let outer = format!("{}-outer", sb.socket);
    let out = |args: &[&str]| {
        let mut c = Command::new("tmux");
        let o = scrub(&mut c).args(["-L", &outer, "-f", "/dev/null"]).args(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    out(&["new-session", "-d", "-x", "160", "-y", "40", &format!("tmux -L {} attach-session -t {here_pane}", sb.socket)]);
    assert!(sb.until(|| sb.tmux(&["list-clients", "-F", "#{@aca_id}"]) == here), "客户端没连上");
    let seen = || out(&["capture-pane", "-p"]);
    out(&["send-keys", &sb.tmux(&["show-options", "-gv", "prefix"])]);
    out(&["send-keys", "b"]);
    assert!(sb.until(|| seen().contains(&caps("⏎ 插入"))), "列表没弹出来:\n{}", seen());
    out(&["send-keys", "/"]);
    out(&["send-keys", "-l", "对面"]);
    out(&["send-keys", "Enter"]);
    out(&["send-keys", "Enter"]);
    let typed = || sb.tmux(&["capture-pane", "-p", "-t", &here_pane]);
    assert!(sb.until(|| typed().contains(&format!("给它传话:aca msg {there} \"…\""))), "引用没插进来:\n{}", typed());
    assert!(typed().contains(&format!("aca 会话 {there}「对面」(Claude,")), "{}", typed());
    assert!(sb.until(|| !seen().contains(&caps("⏎ 插入"))), "列表没关:\n{}", seen());
    out(&["kill-server"]);

    // Another key, then none
    let said = sb.aca(&["tmux", "key", "ref", "prefix", "v"]);
    assert!(said.contains("插入引用:在会话中按 前缀键 + v"), "{said}");
    assert_eq!((keys("b"), keys("v")), (0, 1));
    let said = sb.aca(&["tmux", "key", "ref", "F6"]);
    assert!(said.contains("插入引用:在会话中按 F6"), "{said}");
    assert_eq!((keys("b"), keys("F6")), (0, 3));
    assert!(std::fs::read_to_string(sb.home.join("config/config.toml")).unwrap().contains("ref = \"F6\""));
    let said = sb.aca(&["tmux", "key", "ref", "--none"]);
    assert!(said.contains("插入引用:未绑定按键"), "{said}");
    assert_eq!(sb.tmux(&["list-keys"]).lines().filter(|l| l.contains(" ui --ref ")).count(), 0);
    // The next key wasn't touched
    assert_eq!(sb.tmux(&["list-keys"]).lines().filter(|l| l.contains(" next --hotkey C-M-n ")).count(), 3);
}

/// The same key in the list (opened straight in a terminal, no tmux in between: the prefix key reaches it as is): pick a session,
/// its reference goes into the highlighted one's input box, and the list goes back to that row
#[test]
fn the_ref_key_in_the_list_puts_the_picked_reference_into_the_highlighted_session() {
    let Some(sb) = Sandbox::new("ref-list") else { return };
    let new = |t: &str| {
        let id = sb.aca(&["new", "fake", &sb.proj(), "-t", t]);
        let pane = sb.pane(&id);
        sb.wait_screen(&pane, "fake agent up");
        (id, pane)
    };
    let (_, into) = new("写这里");
    let (there, _) = new("被引用");
    let script = sb.home.join("ui.sh");
    let q = |p: &Path| format!("'{}'", p.display());
    write(
        &script,
        &format!(
            "unset TMUX TMUX_PANE KITTY_WINDOW_ID TERM_PROGRAM SSH_CONNECTION SSH_TTY\n\
             export HOME={home} ACA_HOME={home} ACA_TMUX_SOCKET={sock} ACA_LANG=zh ACA_IME=0 TERM=xterm-256color\n\
             exec {aca} ui\n",
            home = q(&sb.home),
            sock = sb.socket,
            aca = q(Path::new(env!("CARGO_BIN_EXE_aca"))),
        ),
    );
    sb.tmux(&["new-session", "-d", "-s", "ui", "-x", "120", "-y", "30", "/bin/sh", script.to_str().unwrap()]);
    let ui = || sb.tmux(&["capture-pane", "-p", "-t", "ui"]);
    assert!(sb.until(|| ui().contains("快捷键")), "界面没打开");
    std::thread::sleep(std::time::Duration::from_secs(1));
    // send-keys goes straight to the list, past tmux's prefix binding: as a terminal without tmux would
    let keys = |k: &[&str]| {
        for key in k {
            sb.tmux(&["send-keys", "-t", "ui", key]);
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    };
    keys(&["/", "写这里", "Enter", "Escape"]);
    keys(&["C-b", "b", "b"]);
    assert!(sb.until(|| ui().contains("插入到「写这里」")), "{}", ui());
    keys(&["/", "被引用", "Enter", "Enter"]);
    let typed = || sb.tmux(&["capture-pane", "-p", "-t", &into]);
    assert!(sb.until(|| typed().contains(&format!("aca 会话 {there}「被引用」"))), "引用没插进来:\n{}", typed());
    assert!(sb.until(|| ui().contains("已插入「被引用」的引用")), "{}", ui());
}

/// Entering a session from the UI passes first through the one just left (so ^` gets back to it once inside): tmux records the
/// pass-through as attached,
/// and the finished-and-unread one mustn't become seen because of it. Only really switching back to look at it counts as seen
#[test]
fn passing_through_the_session_just_left_on_the_way_in_does_not_count_as_seeing_it() {
    let Some(sb) = Sandbox::new("passby") else { return };
    let left = sb.aca(&["new", "fake", &sb.proj(), "-t", "刚离开的"]);
    let to = sb.aca(&["new", "fake", &sb.proj(), "-t", "要进的"]);
    let (left_pane, to_pane) = (sb.pane(&left), sb.pane(&to));
    sb.hook(&left, &left_pane, "{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"c1\",\"prompt\":\"跑\"}");
    sb.hook(&left, &left_pane, "{\"hook_event_name\":\"Stop\",\"session_id\":\"c1\",\"last_assistant_message\":\"好\"}");
    let state = |id: &str| {
        let rows = sb.json(&["ls", "--json", "--no-screen"]);
        rows.as_array().unwrap().iter().find(|r| r["id"] == id).map(|r| r["state"].as_str().unwrap().to_string()).unwrap()
    };
    assert_eq!(state(&left), "unread");

    // A real client (a pane of another tmux server), attaching by the same command the UI uses to enter a session
    let outer = format!("{}-outer", sb.socket);
    let out = |args: &[&str]| {
        let mut c = Command::new("tmux");
        let o = scrub(&mut c).args(["-L", &outer, "-f", "/dev/null"]).args(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let sid = |pane: &str| sb.tmux(&["display-message", "-p", "-t", pane, "#{session_id}"]);
    let words: Vec<String> = aca_core::tmux::attach_via(&sid(&left_pane), &sid(&to_pane)).iter().map(|a| format!("'{a}'")).collect();
    out(&["new-session", "-d", "-x", "160", "-y", "40", &format!("tmux -L {} {}", sb.socket, words.join(" "))]);
    let at = || sb.tmux(&["list-clients", "-F", "#{@aca_id} #{client_last_session}"]);
    assert!(sb.until(|| at() == format!("{to} 刚离开的")), "客户端没落在要进的那个上、上一个不是路过的:{}", at());
    // tmux recorded it as attached; aca knows that was a pass-through
    assert!(!sb.tmux(&["display-message", "-p", "-t", &left_pane, "#{session_last_attached}"]).is_empty());
    assert_eq!(state(&left), "unread");

    // Really going back to look (that's what ^` does), then leaving: seen. tmux records times in seconds, so it's a second apart from
    // the pass-through
    std::thread::sleep(std::time::Duration::from_millis(1100));
    sb.tmux(&["switch-client", "-l"]);
    assert!(sb.until(|| at().starts_with(&format!("{left} "))), "没切回去:{}", at());
    sb.tmux(&["switch-client", "-l"]);
    assert!(sb.until(|| at().starts_with(&format!("{to} "))), "没切走:{}", at());
    assert_eq!(state(&left), "idle");
    out(&["kill-server"]);
}

/// Ctrl+Q: while you're in a session, agent-deck starts another session, and as soon as its version (which doesn't know aca's
/// sessions, so the key is swallowed) gets bound, tmux switches back to aca's on the spot —
/// a hook on `after-bind-key`. It takes only its own slot in the array; if you bound this key to something else yourself, it's
/// left alone
#[test]
fn ctrl_q_stays_acas_when_agent_deck_binds_its_own_version_over_it() {
    let Some(sb) = Sandbox::new("detachkey") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    sb.wait_screen(&sb.pane(&id), "fake agent up");
    let bound = || sb.tmux(&["list-keys", "-T", "root"]).lines().find(|l| l.contains(" C-q ")).unwrap_or_default().to_string();
    let ours = |l: &str| l.contains("#{?@aca_id,1,0}") && l.contains("send-keys C-q");
    let hooks =
        || sb.tmux(&["show-hooks", "-g"]).lines().filter(|l| l.starts_with("after-bind-key[")).map(str::to_string).collect::<Vec<_>>();
    let deck = ["if-shell", "-F", "#{m:agentdeck_*,#{session_name}}", "detach-client", ""];
    assert!(ours(&bound()), "{}", bound());
    // The binding agent-deck makes when starting a session, both spellings
    for table in [&["-n"][..], &["-T", "root"]] {
        sb.tmux(&[&["bind-key"], table, &["C-q"], &deck[..]].concat());
        assert!(ours(&bound()), "{table:?}:{}", bound());
    }
    // Another key, aca binding it again itself: none of the hook's business
    sb.tmux(&[&["bind-key", "-n", "F6"], &deck[..]].concat());
    assert!(sb.tmux(&["list-keys", "-T", "root"]).lines().any(|l| l.contains(" F6 ") && !l.contains("@aca_id")));
    // Another slot you put on the same hook stays; aca coming again several times still takes just one slot
    sb.tmux(&["set-hook", "-g", "after-bind-key[7]", "set-option -g @mine 1"]);
    sb.aca(&["new", "fake", &sb.proj(), "-t", "第二个"]);
    let h = hooks();
    assert!(h.len() == 2 && h.iter().any(|l| l.contains("@mine")) && h.iter().any(|l| l.contains("hook_argument_0")), "{h:?}");
    // You bound this key to something else yourself: the hook doesn't grab it, nor does the next session creation
    sb.tmux(&["bind-key", "-n", "C-q", "display-message", "mine"]);
    assert!(bound().contains("display-message mine"), "{}", bound());
    sb.aca(&["new", "fake", &sb.proj(), "-t", "第三个"]);
    assert!(bound().contains("display-message mine"), "{}", bound());
    // The tmux server restarts: the key and the hook are both gone, and creating another session restores both
    sb.tmux(&["kill-server"]);
    assert!(hooks().is_empty());
    sb.aca(&["new", "fake", &sb.proj(), "-t", "第四个"]);
    assert!(ours(&bound()) && hooks().len() == 1, "{} {:?}", bound(), hooks());
    sb.tmux(&[&["bind-key", "-n", "C-q"], &deck[..]].concat());
    assert!(ours(&bound()), "{}", bound());
}

/// aca.copy: picks commands out of the original text of the last reply (code blocks, each command in a shell code block, inline
/// code with a space);
/// for sessions whose reply the hook didn't record (adopted and not restarted yet), aca reads it from their transcript
#[test]
fn the_copy_plugin_finds_the_commands_in_the_last_reply_and_reads_the_transcript_when_no_hook_recorded_it() {
    // No copy in the sandbox name: at the end, those four letters are searched for to check everything was deleted
    let Some(sb) = Sandbox::new("yank") else { return };
    if Command::new("fzf").arg("--version").output().is_err() {
        eprintln!("没有 fzf,跳过");
        return;
    }
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    let pane = sb.pane(&id);
    sb.wait_screen(&pane, "fake agent up");
    let out = sb.aca(&["plugin", "install", "aca.copy"]);
    assert!(out.contains("热键:C-M-y 复制命令"), "{out}");
    assert_eq!(sb.aca(&["plugin", "hotkey"]), "C-M-y  aca.copy.pick  复制命令");
    assert!(sb.aca(&["plugin", "key"]).contains("y  aca.copy.pick  复制命令"));
    // No reply yet
    let o = sb.run(&["plugin", "run", "aca.copy.list", "--session", &id]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stdout).contains("还没有回复"), "{o:?}");
    let reply = "先看 `git status`,`--apply` 不算。\n\n1. 编译:\n   ```sh\n   $ cargo build --release\n   $ cp target/release/aca ~/.local/bin/\n   ```\n\n```toml\na = 1\nb = 2\n```\n";
    let stop =
        serde_json::json!({ "hook_event_name": "Stop", "session_id": "c1", "last_assistant_message": reply, "background_tasks": [] });
    sb.hook(&id, &pane, &stop.to_string());
    let out = sb.aca(&["plugin", "run", "aca.copy.list", "--session", &id]);
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    assert_eq!(
        lines,
        [
            "1  cargo build --release  +1 行",
            "2    └ cargo build --release",
            "3    └ cp target/release/aca ~/.local/bin/",
            "4  a = 1  +1 行",
            "5  git status"
        ],
        "{out}"
    );
    // Settings: all inline ones wanted; code blocks not split
    sb.aca(&["plugin", "setting", "aca.copy", "inline", "all"]);
    sb.aca(&["plugin", "setting", "aca.copy", "lines", "false"]);
    let out = sb.aca(&["plugin", "run", "aca.copy.list", "--session", &id]);
    assert!(out.contains("4  --apply") && !out.contains("└"), "{out}");
    // Switched to another conversation, and the hook only reported where the record is, no reply yet: read the last part from the
    // record
    let t = sb.home.join("t.jsonl");
    let said = serde_json::json!({ "type": "assistant", "message": { "model": "m", "content": [{ "type": "text", "text": "装上:\n\n```sh\naca plugin install aca.copy\n```" }] } });
    write(&t, &format!("{said}\n"));
    let id2 = sb.aca(&["new", "fake", &sb.proj(), "-t", "没 hook 的"]);
    let pane2 = sb.pane(&id2);
    let prompt = serde_json::json!({ "hook_event_name": "UserPromptSubmit", "session_id": "c2", "transcript_path": t, "prompt": "hi" });
    sb.hook(&id2, &pane2, &prompt.to_string());
    assert_eq!(sb.aca(&["output", &id2]), "");
    assert_eq!(sb.aca(&["plugin", "run", "aca.copy.list", "--session", &id2]).trim(), "1  aca plugin install aca.copy");
    // A pane was opened once: aca saved a copy of the reply and handed it over
    sb.aca(&["plugin", "setting", "aca.copy", "copy", "cat > /dev/null"]);
    sb.aca(&["plugin", "pane", "aca.copy", "pick", "--session", &id2, "--placement", "window"]);
    let input = sb.home.join("state/plugin-input/aca.copy.pick.txt");
    assert!(input.exists());
    // Remove: the key goes with it; nothing of it is left on this machine (the installed copy, its section in plugins.toml, the
    // saved reply, the empty directory aca made for it)
    sb.aca(&["plugin", "remove", "aca.copy"]);
    assert!(!sb.tmux(&["list-keys"]).contains(" plugin pane "));
    let left: Vec<String> =
        ["config", "state", "data"].iter().flat_map(|d| walk(&sb.home.join(d))).filter(|p| p.contains("copy")).collect();
    assert!(left.is_empty(), "{left:?}");
    assert!(!std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("aca.copy"));
}

/// aca.notify: a desktop notification when a turn finishes, when one needs you, on an error. The command it uses is replaced by
/// a script in the sandbox that only records (tests don't really notify):
/// the title is the mark plus the session name, the body the reply with markdown marks stripped; workers that were sent out
/// notify only with the setting on; if your replacement command fails, the system's is used
#[test]
fn the_notify_plugin_shows_a_desktop_notification_for_your_sessions_and_skips_workers() {
    let Some(sb) = Sandbox::new("notify") else { return };
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("没有 jq,跳过");
        return;
    }
    let out = sb.aca(&["plugin", "install", "aca.notify"]);
    assert!(out.contains("已安装 aca.notify") && out.contains("turn.done"), "{out}");
    let (rec, log) = (sb.home.join("rec.sh"), sb.home.join("shown.txt"));
    write(&rec, &format!("printf '%s|%s\\n' \"$1\" \"$2\" >> '{}'\n", log.display()));
    sb.aca(&["plugin", "setting", "aca.notify", "command", &format!("sh '{}'", rec.display())]);
    let shown = || std::fs::read_to_string(&log).unwrap_or_default();
    let lead = sb.aca(&["new", "fake", &sb.proj(), "-t", "修登录"]);
    let emit = |id: &str, ev: &str, field: &str| sb.aca(&["plugin", "emit", ev, field, "--plugin", "aca.notify", "--session", id]);
    emit(&lead, "turn.done", "last=## 好了\n**两处**都改了,跑 `cargo test`");
    emit(&lead, "agent.waiting", "ask=要删掉 target 吗");
    emit(&lead, "turn.failed", "error=限流了");
    assert_eq!(shown(), "✓ 修登录|好了 两处都改了,跑 cargo test\n? 修登录|要删掉 target 吗\n✕ 修登录|限流了\n");
    // A worker that was sent out: no notification by default (when it finishes it tells whoever sent it), only with the setting on
    let worker = sb.aca(&["new", "fake", &sb.proj(), "-t", "干活的", "--parent", &lead]);
    emit(&worker, "turn.done", "last=做完了");
    assert_eq!(shown().lines().count(), 3, "{}", shown());
    sb.aca(&["plugin", "setting", "aca.notify", "workers", "true"]);
    emit(&worker, "turn.done", "last=做完了");
    assert_eq!(shown().lines().last(), Some("✓ 干活的|做完了"));
    // "Show a test one" in the menu
    assert!(sb.aca(&["plugin", "run", "aca.notify.test"]).contains("shown"));
    assert_eq!(shown().lines().last(), Some("aca|Notify works"));
    // Your replacement command fails (exit code not 0): treated as empty, and the system's is used (here, the fake osascript /
    // notify-send earlier on PATH)
    let (bin, fail) = (sb.home.join("sysbin"), sb.home.join("fail.sh"));
    for prog in ["osascript", "notify-send"] {
        write(&bin.join(prog), &format!("#!/bin/sh\nfor a; do t=$b; b=$a; done\nprintf 'sys|%s|%s\\n' \"$t\" \"$b\" >> '{}'\n", log.display()));
        Command::new("chmod").arg("+x").arg(bin.join(prog)).status().unwrap();
    }
    write(&fail, &format!("printf 'tried|%s\\n' \"$1\" >> '{}'\nexit 1\n", log.display()));
    sb.aca(&["plugin", "setting", "aca.notify", "command", &format!("sh '{}'", fail.display())]);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let o = sb.cmd().env("PATH", &path).args(["plugin", "emit", "turn.done", "last=好了", "--plugin", "aca.notify", "--session", &lead]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let all = shown();
    assert_eq!(all.lines().rev().take(2).collect::<Vec<_>>(), ["sys|✓ 修登录|好了", "tried|✓ 修登录"], "{all}");
}

/// aca.discord takes in what you said in the channel: its script run as the timer runs it, against a `curl` that answers from
/// files (no test talks to Discord). Only your messages go in — oldest first, to the desk, saying which channel they came
/// through; the first run only notes where the channel is; with no desk yet they count as seen and aren't handed on; with
/// a setting missing Discord isn't asked anything; what Discord refuses fails the run and nothing is counted as seen
#[test]
fn the_discord_plugin_hands_what_you_said_in_the_channel_to_the_desk_and_nobody_elses() {
    use serde_json::json;
    let Some(sb) = Sandbox::new("discord-pull") else { return };
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("没有 jq,跳过");
        return;
    }
    sb.aca(&["plugin", "install", "aca.discord"]);
    assert!(sb.aca(&["plugin", "timer"]).contains("aca.discord.pull"));
    // The stand-in: notes how it was called, and prints the file named after the address's query (none: an empty list)
    let (bin, calls, answers) = (sb.home.join("fakebin"), sb.home.join("calls.txt"), sb.home.join("answers"));
    write(
        &bin.join("curl"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nfor a; do url=$a; done\n\
             f='{}'/$(printf '%s' \"${{url#*\\?}}\" | tr -c 'a-z0-9=' '_')\nif [ -f \"$f\" ]; then cat \"$f\"; else echo '[]'; fi\n",
            calls.display(),
            answers.display()
        ),
    );
    Command::new("chmod").arg("+x").arg(bin.join("curl")).status().unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let pull = |what: &str| sb.cmd().env("PATH", &path).args(["plugin", "run", &format!("aca.discord.{what}")]).output().unwrap();
    let asked = || std::fs::read_to_string(&calls).unwrap_or_default();
    let answer = |query: &str, body: &str| write(&answers.join(query), body);
    let seen = || {
        let file = walk(&sb.home).into_iter().find(|p| p.ends_with("/seen-111"));
        file.map(|p| std::fs::read_to_string(p).unwrap().trim().to_string()).unwrap_or_default()
    };
    let msg = |id: &str, by: &str, text: &str| json!({ "id": id, "author": { "id": by }, "content": text });

    // Not set up to read: nothing is asked of Discord
    assert!(pull("pull").status.success() && asked().is_empty());
    for (key, value) in [("token", "T0KEN"), ("channel", "111"), ("user", "42")] {
        assert!(pull("pull").status.success() && asked().is_empty(), "{key}");
        sb.aca(&["plugin", "setting", "aca.discord", key, value]);
    }
    // The first run takes nothing in: it notes the channel's latest message
    answer("limit=1", &json!([msg("100", "42", "以前说的")]).to_string());
    assert!(pull("pull").status.success());
    assert_eq!(seen(), "100");
    assert!(asked().contains("Authorization: Bot T0KEN") && asked().contains("https://discord.com/api/v10/channels/111/messages?limit=1"));
    // No desk yet: what came since counts as seen and goes nowhere, so the first desk isn't handed all of it at once
    answer("after=100_limit=100", &json!([msg("103", "42", "还没有总台时说的"), msg("101", "42", "更早的")]).to_string());
    let o = pull("pull");
    assert!(o.status.success(), "{o:?}");
    assert_eq!(seen(), "103");
    let dir = sb.home.join("asks");
    std::fs::create_dir_all(&dir).unwrap();
    add_config(&sb, &format!("[scratch]\ndir = \"{}\"\n", dir.display()));
    let desk = sb.aca(&["new", "fake", &dir.to_string_lossy(), "-g", "随手问", "-t", "总台"]);
    sb.aca(&["group", "dir", "随手问", &dir.to_string_lossy()]);
    sb.aca(&["group", "router", "随手问", &desk]);
    let taken = || -> Vec<(String, String, String)> {
        let lines = sb.aca(&["events", "-n", "200"]);
        let events = lines.lines().map(|l| serde_json::from_str::<Value>(l).unwrap());
        let sent = events.filter(|e| e["event"] == "message.sent" && e["session"] == desk.as_str());
        let field = |e: &Value, k: &str| e[k].as_str().unwrap_or("").to_string();
        sent.map(|e| (field(&e, "from"), field(&e, "via"), field(&e, "text"))).collect()
    };
    assert!(taken().is_empty());
    // With a desk: yours go to it oldest first, as written (quotes, lines, something that looks like an option); somebody
    // else's, a bot's, and one with no words (an attachment) don't
    let mut bot = msg("105", "42", "bot 发的");
    bot["author"]["bot"] = json!(true);
    let batch = json!([
        msg("107", "42", "--json"),
        msg("106", "42", ""),
        bot,
        msg("104", "7", "别人说的:rm -rf ~"),
        msg("102", "42", "帮我看 \"机票\"\n周二走")
    ]);
    answer("after=103_limit=100", &batch.to_string());
    let o = pull("pull");
    assert!(o.status.success(), "{o:?}");
    let you = |text: &str| ("you".to_string(), "Discord".to_string(), text.to_string());
    assert_eq!(taken(), [you("帮我看 \"机票\"\n周二走"), you("--json")]);
    assert_eq!(seen(), "107");
    // Discord says no (a wrong token): the run fails saying what it said, and nothing more counts as seen
    answer("after=107_limit=100", r#"{"message":"401: Unauthorized","code":0}"#);
    let o = pull("pull");
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stdout).contains("401: Unauthorized"), "{o:?}");
    assert_eq!((seen(), taken().len()), ("107".to_string(), 2));
    // The check for setting it up: what the bot can read, and whether it sees your words
    answer("limit=5", &json!([msg("107", "42", "--json")]).to_string());
    let said = String::from_utf8_lossy(&pull("check").stdout).into_owned();
    assert!(said.contains("reads the channel: yes") && said.contains("sees what you wrote: yes"), "{said}");
    answer("limit=5", &json!([msg("107", "42", "")]).to_string());
    assert!(String::from_utf8_lossy(&pull("check").stdout).contains("Message Content Intent"));
}

/// Every path (files and directories) under a directory
fn walk(dir: &Path) -> Vec<String> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        out.push(e.path().to_string_lossy().into_owned());
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        }
    }
    out
}

/// Add one more section to the config
fn add_config(sb: &Sandbox, more: &str) {
    let cfg = sb.home.join("config/config.toml");
    write(&cfg, &(std::fs::read_to_string(&cfg).unwrap() + more));
}

/// `aca group ls --json` marks the scratch group — the one `aca msg desk` takes its router from — with `"scratch": true`,
/// and no other group carries the field. It goes by the directory in config.toml, not by the name
#[test]
fn group_ls_json_marks_the_scratch_group_and_only_it() {
    use serde_json::json;
    let Some(sb) = Sandbox::new("group-scratch") else { return };
    let dir = sb.home.join("asks");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_s = dir.to_string_lossy().into_owned();
    sb.aca(&["new", "fake", &sb.proj(), "-g", "甲组", "-t", "甲一"]);
    sb.aca(&["group", "create", "随手问", &dir_s]);
    let marked = || -> Vec<(String, Value)> {
        let listed = sb.json(&["group", "ls", "--json"]);
        let groups = listed.as_array().unwrap();
        assert_eq!(groups.len(), 2, "{listed}");
        groups.iter().filter_map(|g| Some((g["path"].as_str().unwrap().to_string(), g.get("scratch")?.clone()))).collect()
    };
    // No scratch directory set: none is marked
    assert_eq!(marked(), []);
    // Set, written another way than the group's directory
    add_config(&sb, &format!("[scratch]\ndir = \"{dir_s}/\"\n"));
    assert_eq!(marked(), [("随手问".to_string(), json!(true))]);
    sb.aca(&["group", "rename", "随手问", "loose"]);
    assert_eq!(marked(), [("loose".to_string(), json!(true))]);
    // Its router is the desk
    assert_eq!(sb.run(&["msg", "desk", "hi"]).status.code(), Some(3));
    let desk = sb.aca(&["new", "fake", &dir_s, "-g", "loose", "-t", "总台"]);
    sb.aca(&["group", "router", "loose", &desk]);
    let listed = sb.json(&["group", "ls", "--json"]);
    let scratch: Vec<&Value> = listed.as_array().unwrap().iter().filter(|g| g.get("scratch").is_some()).collect();
    assert_eq!((scratch.len(), &scratch[0]["router"]), (1, &json!(desk)));
    sb.aca(&["msg", "desk", "hi"]);
    assert_eq!(sb.json(&["inbox", &desk, "--json"])[0]["text"], "hi");
}

#[test]
fn the_opening_message_goes_in_as_a_launch_argument_and_only_the_first_time() {
    let Some(sb) = Sandbox::new("opening") else { return };
    // A fake agent: records the arguments it got (one argument per piece, separated by \0), one record per start
    let args = sb.home.join("args.bin");
    let script = sb.home.join("rec.sh");
    write(
        &script,
        &format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\0' \"$a\"; done > '{}'\necho recorded\nexec sleep 600\n", args.display()),
    );
    // rec is started the Claude way (takes the opening message as a start argument), plain is declarative (doesn't)
    add_config(
        &sb,
        &format!(
            "[tools.rec]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{0}\"]\n[tools.plain]\ncmd = [\"/bin/sh\", \"{0}\"]\n",
            script.display()
        ),
    );
    let got = || -> Vec<String> {
        let raw = std::fs::read(&args).unwrap_or_default();
        raw.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).filter(|a| !a.is_empty()).collect()
    };

    // A brief: starts with a dash, many lines, with quotes, over thirty thousand bytes — too long for one tmux command, so it goes in
    // through a file
    let brief = format!("- 第一条 \"带引号\" 'x'\n# Task\n{}\n最后一行", "改这个文件 ".repeat(2500));
    assert!(brief.len() > 30_000);
    let t = std::time::Instant::now();
    let o = sb.run(&["new", "rec", &sb.proj(), "-t", "带话", "--say", &brief]);
    assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
    // No waiting for it to start and go idle (the typing-in path has to wait)
    assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
    let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let pane = sb.pane(&id);
    assert!(sb.wait_screen(&pane, "recorded").contains("recorded"));
    let a = got();
    assert_eq!(a[a.len() - 2..], ["--".to_string(), brief.clone()]);
    // That file is deleted once read
    assert!(walk(&sb.home.join("state/opening")).is_empty());

    // Start again: the message isn't said a second time
    sb.aca(&["stop", &id]);
    std::fs::remove_file(&args).unwrap();
    sb.aca(&["start", &id]);
    assert!(sb.until(|| args.exists()));
    assert!(sb.wait_screen(&pane, "recorded").contains("recorded"));
    assert!(got().iter().all(|x| *x != brief && x != "--"), "{:?}", got().last());

    // A single English word would be taken as a subcommand: not passed; typed in once it's up
    let o = sb.run(&["new", "rec", &sb.proj(), "-t", "一个词", "--say", "resume"]);
    assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
    let word = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert!(got().iter().all(|x| x != "resume" && x != "--"), "{:?}", got());
    assert!(sb.wait_screen(&sb.pane(&word), "resume").contains("resume"));

    // The harness that doesn't take it: typed in too
    let o = sb.run(&["new", "plain", &sb.proj(), "-t", "打字", "--say", "看一下 README"]);
    assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
    let typed = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert!(got().iter().all(|x| x != "看一下 README" && x != "--"), "{:?}", got());
    assert!(sb.wait_screen(&sb.pane(&typed), "看一下 README").contains("看一下 README"));

    // Registered only, not started: the message has nowhere to go, so it says so, and the session is created anyway
    let o = sb.run(&["new", "rec", &sb.proj(), "-t", "先不起", "--hold", "--say", "两个 词"]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stderr).contains("开场消息未发送"), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(sb.json(&["show", "先不起", "--json"])["state"], "stopped");
}

#[test]
fn the_folder_trust_question_is_answered_at_launch_only_when_the_switch_is_on() {
    let Some(sb) = Sandbox::new("trust") else { return };
    // A fake agent: shows Claude's "do you trust this directory" box (cursor on No), reads until Enter, and records the keys pressed
    let keys = sb.home.join("keys.txt");
    let script = sb.home.join("gate.sh");
    write(
        &script,
        &format!(
            "#!/bin/sh\n\
             printf ' Accessing workspace:\\n\\n /p/api\\n\\n ❯ No, exit\\n   Yes, I trust this folder\\n\\n Enter to confirm · Esc to cancel\\n'\n\
             IFS= read -r k\n\
             if [ \"$k\" = \"$(printf '\\033[B')\" ]; then echo down-enter; else printf '%s' \"$k\" | od -c; fi > '{}'\n\
             echo trusted\n\
             exec sleep 600\n",
            keys.display()
        ),
    );
    add_config(&sb, &format!("[tools.gate]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", script.display()));

    // Switch off: it stops there, waiting for you
    let off = sb.aca(&["new", "gate", &sb.proj(), "-t", "没开"]);
    assert!(sb.wait_screen(&sb.pane(&off), "Enter to confirm").contains("Yes, I trust this folder"));
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert!(!keys.exists());
    assert_eq!(sb.json(&["show", &off, "--json"])["state"], "ask");
    sb.aca(&["rm", &off]);

    // On: once it's up, it goes from No to Yes and presses Enter for you
    add_config(&sb, "[trust]\nfolder = true\n");
    let on = sb.aca(&["new", "gate", &sb.proj(), "-t", "开了"]);
    assert!(sb.until(|| keys.exists()), "{}", sb.tmux(&["capture-pane", "-p", "-t", &sb.pane(&on)]));
    assert_eq!(std::fs::read_to_string(&keys).unwrap().trim(), "down-enter");
    let pane = sb.pane(&on);
    assert!(sb.wait_screen(&pane, "trusted").contains("trusted"));

    // Start again (in place, same pane): answered for you the same way
    sb.aca(&["stop", &on]);
    std::fs::remove_file(&keys).unwrap();
    sb.aca(&["start", &on]);
    assert!(sb.until(|| keys.exists()), "{}", sb.tmux(&["capture-pane", "-p", "-t", &pane]));
    assert_eq!(std::fs::read_to_string(&keys).unwrap().trim(), "down-enter");
}

#[test]
fn a_typed_opening_message_waits_until_the_screen_has_settled_after_a_startup_gate() {
    let Some(sb) = Sandbox::new("settle") else { return };
    sb.screen_tools();
    // A single English word: scr (the Claude way) can only type it in too. `aca new` returns only after it's typed in, so it runs on
    // the side
    let mut new =
        sb.cmd().args(["new", "scr", &sb.proj(), "-t", "等画稳", "--say", "resume"]).stderr(std::process::Stdio::piped()).spawn().unwrap();
    // While the other side is writing the registry, this side can't read it (locked): treat it as not there yet, ask again
    let listed = || {
        let rows: Value = serde_json::from_slice(&sb.run(&["ls", "--json"]).stdout).unwrap_or_default();
        rows.as_array().and_then(|a| a.first()).and_then(|r| r["id"].as_str()).unwrap_or("").to_string()
    };
    assert!(sb.until(|| !listed().is_empty()));
    let id = listed();
    let pane = sb.pane(&id);
    // The gate at start: counts as needing you, nothing typed
    sb.set_screen(&id, &pane, " Accessing workspace:\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(new.try_wait().unwrap().is_none());
    // Someone answered it: it's no longer asking, but the UI is still drawing frame after frame. Text typed in now reaches nobody
    // (here: erased as soon as the next frame is drawn)
    for i in 0..14 {
        write(&sb.home.join("screens").join(&id), &format!("Loading… {i}"));
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    // Drawn and settled, empty: only now type
    write(&sb.home.join("screens").join(&id), "● ready\n\n❯ ");
    let o = new.wait_with_output().unwrap();
    assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
    let screen = sb.wait_screen(&pane, "resume");
    assert!(screen.contains("ready") && screen.contains("resume"), "{screen}");
}

#[test]
fn restart_as_another_account_carries_the_conversation_over_and_back() {
    let Some(sb) = Sandbox::new("rehome") else { return };
    // A fake agent: at start, writes its config directory and arguments into run-<SESSION-ID>, one per line
    let script = sb.home.join("rec.sh");
    write(
        &script,
        &format!(
            "#!/bin/sh\n{{ echo \"home=$CLAUDE_CONFIG_DIR$CODEX_HOME\"; printf '%s\\n' \"$@\"; }} > '{}/run-'\"$ACA_SESSION_ID\"\nexec sleep 600\n",
            sb.home.display()
        ),
    );
    let (ca, cb, xa, xb) = (sb.home.join("ca"), sb.home.join("cb"), sb.home.join("xa"), sb.home.join("xb"));
    write(&xa.join("config.toml"), "model = \"m-a\"\n");
    write(&xb.join("config.toml"), "model = \"m-b\"\n");
    let cfg = sb.home.join("config/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap()
        + &format!(
            "[tools.rc]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{s}\"]\n[tools.rx]\nkind = \"codex\"\ncmd = [\"/bin/sh\", \"{s}\"]\n\
             [accounts.claude]\na = \"{}\"\nb = \"{}\"\n[accounts.codex]\nx = \"{}\"\ny = \"{}\"\n",
            ca.display(),
            cb.display(),
            xa.display(),
            xb.display(),
            s = script.display()
        );
    write(&cfg, &text);
    // The config directory and arguments of this start; delete the old one first and wait for the new start to write it
    let relaunch = |id: &str, args: &[&str]| -> (String, Vec<String>, String) {
        let f = sb.home.join(format!("run-{id}"));
        let _ = std::fs::remove_file(&f);
        let said = sb.aca(args);
        for _ in 0..50 {
            if let Ok(t) = std::fs::read_to_string(&f)
                && t.ends_with('\n')
            {
                let mut lines = t.lines().map(String::from);
                let home = lines.next().unwrap().trim_start_matches("home=").to_string();
                return (home, lines.collect(), said);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("{id} 没起来");
    };
    let after = |args: &[String], flag: &str| args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone());
    let read = |p: &Path| std::fs::read_to_string(p).unwrap();

    // Claude: the transcript, the subagents' records and the /rewind checkpoints are all in a's config directory
    let id = sb.aca(&["new", "rc:a", &sb.proj()]);
    let pane = sb.json(&["show", &id, "--json"])["pane"].as_str().unwrap().to_string();
    let rel = Path::new("projects/-proj/c1.jsonl");
    write(&ca.join(rel), "{\"type\":\"user\"}\n");
    write(&ca.join("projects/-proj/c1/subagents/agent-1.jsonl"), "{\"type\":\"user\"}\n");
    write(&ca.join("file-history/c1/f@v1"), "x");
    let start =
        |t: &Path| format!("{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"c1\",\"transcript_path\":\"{}\"}}", t.display());
    sb.hook(&id, &pane, &start(&ca.join(rel)));

    // Switch to b: the three are copied to the same place under b, and it starts in place with b's directory, resuming c1
    let (home, args, said) = relaunch(&id, &["restart", &id, "--as", "b"]);
    assert!(said.contains("已切换到 rc:b") && said.contains("3 个文件"), "{said}");
    assert_eq!((home.as_str(), after(&args, "--resume").as_deref()), (cb.to_str().unwrap(), Some("c1")), "{args:?}");
    assert_eq!(read(&cb.join(rel)), read(&ca.join(rel)));
    assert!(cb.join("projects/-proj/c1/subagents/agent-1.jsonl").is_file() && cb.join("file-history/c1/f@v1").is_file());
    let row = sb.json(&["show", &id, "--json"]);
    assert_eq!((row["tool"].as_str(), row["account"].as_str(), row["pane"].as_str()), (Some("rc"), Some("b"), Some(pane.as_str())));
    assert!(sb.event_names().iter().any(|e| e == "session.rehomed"));

    // One more turn on b, then back to a: a's copy is the beginning of b's, so it's overwritten directly, no backup kept
    write(&cb.join(rel), "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n");
    sb.hook(&id, &pane, &start(&cb.join(rel)));
    let (home, _, _) = relaunch(&id, &["restart", &id, "--as", "rc:a"]);
    assert_eq!(home, ca.to_str().unwrap());
    assert_eq!(read(&ca.join(rel)), read(&cb.join(rel)));
    let names =
        |d: &Path| std::fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect::<Vec<_>>();
    assert!(!names(&ca.join("projects/-proj")).iter().any(|n| n.contains(".bak-")), "{:?}", names(&ca.join("projects/-proj")));

    // b's copy doesn't match (not copied over from here): keep a backup first, then overwrite
    write(&cb.join(rel), "{\"type\":\"别的\"}\n");
    relaunch(&id, &["restart", &id, "--as", "b"]);
    assert_eq!(read(&cb.join(rel)), read(&ca.join(rel)));
    assert!(names(&cb.join("projects/-proj")).iter().any(|n| n.starts_with("c1.jsonl.bak-")), "{:?}", names(&cb.join("projects/-proj")));

    // Ones that can't switch: another harness's tool, already on it, together with --fresh
    let fails = |args: &[&str]| {
        let o = sb.run(args);
        assert!(!o.status.success(), "{args:?}");
        String::from_utf8_lossy(&o.stderr).into_owned()
    };
    assert!(fails(&["restart", &id, "--as", "rx:x"]).contains("H 交接"));
    assert!(fails(&["restart", &id, "--as", "b"]).contains("已是 rc:b"));
    assert!(fails(&["restart", &id, "--as", "a", "--fresh"]).contains("--fresh"));

    // Codex: records are under sessions/<DATE>/; the two accounts' default models differ, so the one used before is passed
    let cx = sb.aca(&["new", "rx:x", &sb.proj()]);
    let cpane = sb.json(&["show", &cx, "--json"])["pane"].as_str().unwrap().to_string();
    let rollout = Path::new("sessions/2026/10/14/rollout-2026-10-14T09-00-00-th-1.jsonl");
    write(
        &xa.join(rollout),
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"th-1\"}}\n{\"type\":\"turn_context\",\"payload\":{\"model\":\"m-a\"}}\n",
    );
    let begin = serde_json::json!({"hook_event_name": "SessionStart", "session_id": "th-1", "transcript_path": xa.join(rollout)});
    sb.hook_as("codex", &cx, &cpane, &begin.to_string());
    let (home, args, said) = relaunch(&cx, &["restart", &cx, "--as", "y"]);
    assert!(said.contains("-m m-a"), "{said}");
    assert_eq!(home, xb.to_str().unwrap());
    assert_eq!(
        (args.first().map(String::as_str), args.get(1).map(String::as_str), after(&args, "-m").as_deref()),
        (Some("resume"), Some("th-1"), Some("m-a"))
    );
    assert_eq!(read(&xb.join(rollout)), read(&xa.join(rollout)));
    assert_eq!(sb.json(&["show", &cx, "--json"])["args"], serde_json::json!(["-m", "m-a"]));

    // Back to the original account: that's its default, so -m is dropped
    let (home, args, _) = relaunch(&cx, &["restart", &cx, "--as", "x"]);
    assert_eq!((home.as_str(), after(&args, "-m")), (xa.to_str().unwrap(), None));
    assert_eq!(sb.json(&["show", &cx, "--json"])["args"], serde_json::json!([]));
}

/// The UI opened under a shell (sh ignores hangups, the same as under zsh in kitty), and the whole pane closed = kitty closed
/// that window.
/// Before: printing to the gone terminal failed, the cleanup after the failure printed and failed again, and the program was
/// aborted (134), leaving nothing behind (the 2026-10-14 incident).
/// Now: a normal exit (0), with exit.log noting the terminal was gone; a closed terminal is common, so the next UI start doesn't
/// mention it
#[test]
fn closing_the_terminal_under_the_interface_quits_quietly() {
    let Some(sb) = Sandbox::new("hangup") else { return };
    let status = sb.home.join("status");
    let script = sb.home.join("ui.sh");
    let q = |p: &Path| format!("'{}'", p.display());
    write(
        &script,
        &format!(
            "trap '' HUP\n\
             unset TMUX TMUX_PANE KITTY_WINDOW_ID TERM_PROGRAM SSH_CONNECTION SSH_TTY\n\
             export HOME={home} ACA_HOME={home} ACA_TMUX_SOCKET={sock} ACA_LANG=zh ACA_IME=0 TERM=xterm-256color\n\
             {aca} ui\n\
             echo $? >> {status}\n",
            home = q(&sb.home),
            sock = sb.socket,
            aca = q(Path::new(env!("CARGO_BIN_EXE_aca"))),
            status = q(&status),
        ),
    );
    // Open a UI and wait for want to appear on screen
    let open = |name: &str, want: &str| {
        sb.tmux(&["new-session", "-d", "-s", name, "-x", "120", "-y", "30", "/bin/sh", script.to_str().unwrap()]);
        let mut screen = String::new();
        for _ in 0..100 {
            screen = sb.tmux(&["capture-pane", "-p", "-t", name]);
            if screen.contains(want) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("没等到「{want}」:\n{screen}");
    };
    // aca's exit codes after closing (one line each time)
    let codes = |n: usize| {
        assert!(sb.until(|| std::fs::read_to_string(&status).is_ok_and(|s| s.lines().count() >= n)), "aca 没退出");
        std::fs::read_to_string(&status).unwrap()
    };
    let ui = sb.home.join("state/ui");

    open("first", "快捷键");
    sb.tmux(&["kill-pane", "-t", "first"]);
    assert_eq!(codes(1), "0\n");
    let log = std::fs::read_to_string(ui.join("exit.log")).unwrap();
    assert!(log.contains("终端已关闭,界面随之退出(终端)"), "{log}");
    assert!(!ui.join("last-exit").exists());

    // Opened next time: the bottom bar doesn't mention it
    open("second", "快捷键");
    let screen = sb.tmux(&["capture-pane", "-p", "-t", "second"]);
    assert!(!screen.contains("上次界面"), "{screen}");
    sb.tmux(&["kill-pane", "-t", "second"]);
    assert_eq!(codes(2), "0\n0\n");
    assert_eq!(std::fs::read_to_string(ui.join("exit.log")).unwrap().matches("终端已关闭").count(), 2);
}

/// What an open UI sends to the background without waiting (trust-watch at session start, plugins subscribed to events) must be
/// reaped when it exits.
/// Before, it was left alone: with a UI open for days, each session started / deleted under it added one more zombie (37 seen
/// on 2026-10-25)
#[test]
fn the_interface_reaps_what_it_leaves_running_in_the_background() {
    let Some(sb) = Sandbox::new("reap") else { return };
    add_config(&sb, "[trust]\nfolder = true\n");
    let heard = sb.home.join("heard");
    let dir = sb.home.join("hear");
    write(
        &dir.join("aca-plugin.toml"),
        &format!(
            "id = \"test.hear\"\nname = \"Hear\"\nversion = \"0.1.0\"\n\
             [[events]]\non = \"session.*\"\ncommand = [\"sh\", \"-c\", \"echo $ACA_PLUGIN_EVENT >> '{}'\"]\n",
            heard.display()
        ),
    );
    sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "x"]);
    sb.hook(&id, &sb.pane(&id), r#"{"hook_event_name":"Stop","session_id":"r1"}"#);
    let script = sb.home.join("ui.sh");
    let q = |p: &Path| format!("'{}'", p.display());
    write(
        &script,
        &format!(
            "unset TMUX TMUX_PANE KITTY_WINDOW_ID TERM_PROGRAM SSH_CONNECTION SSH_TTY\n\
             export HOME={home} ACA_HOME={home} ACA_TMUX_SOCKET={sock} ACA_LANG=zh ACA_IME=0 TERM=xterm-256color\n\
             exec {aca} ui\n",
            home = q(&sb.home),
            sock = sb.socket,
            aca = q(Path::new(env!("CARGO_BIN_EXE_aca"))),
        ),
    );
    sb.tmux(&["new-session", "-d", "-s", "ui", "-x", "120", "-y", "30", "/bin/sh", script.to_str().unwrap()]);
    assert!(sb.until(|| sb.tmux(&["capture-pane", "-p", "-t", "ui"]).contains("快捷键")), "界面没打开");
    let ui = sb.tmux(&["display-message", "-p", "-t", "ui", "#{pane_pid}"]);

    // Restart it from the UI: that sends out a trust-watch and a plugin event hook (aca new already heard one at start).
    // The UI ignores keys in the first moment after opening (see STARTUP_QUIET in ui.rs)
    std::thread::sleep(std::time::Duration::from_secs(1));
    sb.tmux(&["send-keys", "-t", "ui", "R"]);
    let started = || std::fs::read_to_string(&heard).unwrap_or_default().matches("session.started").count() == 2;
    assert!(sb.until(started), "{}", sb.tmux(&["capture-pane", "-p", "-t", "ui"]));
    // trust-watch exits once the agent reports a hook, no need to wait half a minute
    sb.hook(&id, &sb.pane(&id), r#"{"hook_event_name":"SessionStart","session_id":"r1"}"#);

    // The child processes under the UI (state): wait for running ones to exit; once all the rest have exited (Z), whether they were
    // reaped shows
    let kids = || -> Vec<String> {
        let o = Command::new("ps").args(["-axo", "ppid=,stat="]).output().unwrap();
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                (w.next()? == ui).then(|| w.next().unwrap_or_default().to_string())
            })
            .collect()
    };
    for _ in 0..150 {
        if kids().iter().all(|s| s.starts_with('Z')) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    let left = kids();
    assert!(!left.iter().any(|s| s.starts_with('Z')), "界面底下挂着僵尸:{left:?}");
}

/// Printing into a pipe whose reader has left (`aca ls | head -1`, quitting `aca help | less` before the end): no panic, exit
/// 141 (the same as being killed by SIGPIPE)
#[test]
fn printing_into_a_pipe_nobody_reads_any_more_exits_quietly() {
    let Some(sb) = Sandbox::new("epipe") else { return };
    // The reading end is closed first: any write is EPIPE, no racing against time
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let o = sb.cmd().arg("help").stdout(writer).output().unwrap();
    assert_eq!(o.status.code(), Some(141), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn a_sandbox_takes_work_and_messages_from_other_sessions_only_when_it_says_delegate() {
    let Some(sb) = Sandbox::new("cage-delegate") else { return };
    let cfg = sb.home.join("config/config.toml");
    let base = std::fs::read_to_string(&cfg).unwrap();
    let cage = |more: &str| {
        write(
            &cfg,
            &format!(
                "{base}[sandbox.work]\nssh = \"aca-e2e-nowhere\"\ndirs = [\"{}\"]\nworkspace = \"{}\"\n{more}\
                 [accounts.claude]\nteam = {{ sandbox = \"work\" }}\n",
                sb.proj(),
                sb.home.join("cage").display()
            ),
        )
    };
    cage("");
    let me = sb.aca(&["new", "fake", &sb.proj(), "--hold"]);
    // You start one inside yourself: fine
    let inside = sb.aca(&["new", "fake:team", &sb.proj(), "--hold"]);

    // Closed: another session can't start one inside, nor talk to one there
    let o = sb.as_session(&me, &["new", "fake:team", &sb.proj(), "--hold"], "");
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("delegate = true"), "{o:?}");
    // Saying the new one is nobody's child doesn't change whose command starts it; from outside any session it is yours
    let o = sb.as_session(&me, &["new", "fake:team", &sb.proj(), "--hold", "--parent", "-"], "");
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("delegate = true"), "{o:?}");
    sb.aca(&["new", "fake:team", &sb.proj(), "--hold", "--parent", "-"]);
    let o = sb.as_session(&me, &["msg", &inside, "看一下"], "");
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("delegate = true"), "{o:?}");

    // delegate = true: both go in, and the worker is that session's child as anywhere else
    cage("delegate = true\n");
    let o = sb.as_session(&me, &["new", "fake:team", &sb.proj(), "--hold"], "");
    assert!(o.status.success(), "{o:?}");
    let worker = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert_eq!(sb.json(&["show", &worker, "--json"])["parent"], me.as_str());
    let o = sb.as_session(&me, &["msg", &inside, "看一下"], "");
    assert!(o.status.success(), "{o:?}");
}

#[test]
fn a_local_codex_terminal_finishes_wait_and_notifies_its_parent_once() {
    let Some(sb) = Sandbox::new("codex-terminal") else { return };
    sb.screen_tools();
    let parent = sb.aca(&["new", "shell", &sb.proj(), "--hold", "--parent", "-"]);
    let id = sb.aca(&["new", "fcx", &sb.proj(), "--parent", &parent, "--notify", "done"]);
    let pane = sb.pane(&id);
    let frame = |report: &str, label: &str| {
        write(&sb.home.join("screens").join(&id), &format!("\x1b]7501;{report}\x07{label}\n› "));
        assert!(sb.wait_screen(&pane, label).contains(label));
    };
    let count = |name: &str| sb.event_names().iter().filter(|n| n.as_str() == name).count();
    let state = || sb.json(&["show", &id, "--json"])["state"].clone();
    let wait = |seconds| sb.run(&["wait", &id, "--timeout", seconds]);
    frame("state=done", "startup");
    assert!(sb.until(|| state() == "idle"));
    assert_eq!(wait("1").status.code(), Some(124));
    frame("state=working", "first submit");
    assert!(sb.until(|| state() == "running"));
    frame("id=subtask:state=done", "subtask ended");
    assert_eq!(state(), "running");
    frame("state=blocked:kind=permission", "needs permission");
    assert!(sb.until(|| state() == "ask"));
    frame("state=working", "approved");
    assert!(sb.until(|| state() == "running"));
    frame("state=done", "first reply");
    assert!(sb.until(|| count("turn.done") == 1));
    let result = wait("4");
    assert_eq!(result.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&result.stdout).trim(), format!("{id} 完成一轮"));
    sb.hook_as("codex", &id, &pane, r#"{"hook_event_name":"Stop","last_assistant_message":"first reply"}"#);
    frame("state=done", "redrawn");
    assert_eq!((count("turn.started"), count("turn.done")), (1, 1));
    let notices = sb.json(&["inbox", &parent, "--json"]);
    assert_eq!(notices.as_array().unwrap().iter().filter(|m| m["from"] == id && m["what"] == "turn.done").count(), 1);
    frame("state=working", "second submit");
    assert!(sb.until(|| state() == "running"));
    frame("state=idle", "interrupted");
    assert!(sb.until(|| state() == "idle"));
    assert_eq!(wait("4").status.code(), Some(124));
    assert_eq!(count("turn.done"), 1);
}

#[test]
fn a_worker_delegated_into_a_sandbox_reports_its_turn_back_without_hooks() {
    let Some(sb) = Sandbox::new("cage-turn") else { return };
    // The "cage": an ssh that runs the command right here, and a Claude inside that only speaks OSC 7501 — idle, then working
    // and done for each line it reads — and writes its reply into its config directory in the workspace
    let bin = sb.home.join("bin");
    let ws = sb.home.join("cage");
    write(&bin.join("ssh"), "#!/bin/sh\nwhile [ \"$#\" -gt 0 ] && [ \"$1\" != -- ]; do shift; done\nexec sh -c \"$2\"\n");
    write(
        &bin.join("claude"),
        &format!(
            "#!/bin/sh\nsid=x\nwhile [ \"$#\" -gt 0 ]; do [ \"$1\" = --session-id ] && sid=$2; shift; done\n\
             d='{}/claude/projects/p'; mkdir -p \"$d\"\n\
             st() {{ printf '\\033]7501;state=%s\\033\\\\' \"$1\"; }}\n\
             st idle; echo 'fake claude up'\n\
             while IFS= read -r line; do st working; sleep 1\n\
             printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"做完了\"}}]}}}}' >> \"$d/$sid.jsonl\"\n\
             st done; done\n",
            ws.display()
        ),
    );
    for f in ["ssh", "claude"] {
        assert!(Command::new("chmod").arg("+x").arg(bin.join(f)).status().unwrap().success());
    }
    add_config(
        &sb,
        &format!(
            "[sandbox.work]\nssh = \"work\"\ndirs = [\"{}\"]\nworkspace = \"{}\"\ndelegate = true\n[accounts.claude]\nteam = {{ sandbox = \"work\" }}\n",
            sb.proj(),
            ws.display()
        ),
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let run = |me: Option<&str>, args: &[&str]| {
        let mut c = sb.cmd();
        c.env("PATH", &path).args(args);
        if let Some(me) = me {
            c.env("ACA_SESSION_ID", me);
        }
        let o = c.output().unwrap();
        assert!(o.status.success(), "aca {args:?}:{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let lead = run(None, &["new", "fake", &sb.proj(), "--hold"]);
    let w = run(Some(&lead), &["new", "claude:team", &sb.proj(), "--notify", "done", "--say", "做个事"]);
    let o = sb.cmd().env("PATH", &path).env("ACA_SESSION_ID", &lead).args(["wait", &w, "--timeout", "20"]).output().unwrap();
    assert_eq!(
        (o.status.code(), String::from_utf8_lossy(&o.stdout).trim()),
        (Some(0), format!("{w} 完成一轮:「做完了」").as_str()),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let inbox = sb.json(&["inbox", &lead, "--json"]);
    assert_eq!((inbox[0]["what"].as_str(), inbox[0]["from"].as_str()), (Some("turn.done"), Some(w.as_str())), "{inbox}");
}

/// The list in a terminal of its own (inside the sandbox's tmux server), with `env` added to its environment: keys go in with
/// send-keys, its screen comes back with `ui()`
struct Ui<'a> {
    sb: &'a Sandbox,
}

impl<'a> Ui<'a> {
    fn open(sb: &'a Sandbox, env: &str) -> Ui<'a> {
        Ui::open_sized(sb, env, 120, 30)
    }

    /// In a terminal this many columns and rows
    fn open_sized(sb: &'a Sandbox, env: &str, w: u16, h: u16) -> Ui<'a> {
        let script = sb.home.join("ui.sh");
        let q = |p: &Path| format!("'{}'", p.display());
        write(
            &script,
            &format!(
                "unset TMUX TMUX_PANE KITTY_WINDOW_ID TERM_PROGRAM SSH_CONNECTION SSH_TTY\n\
                 export HOME={home} ACA_HOME={home} ACA_TMUX_SOCKET={sock} ACA_LANG=zh ACA_IME=0 TERM=xterm-256color {env}\n\
                 exec {aca} ui\n",
                home = q(&sb.home),
                sock = sb.socket,
                aca = q(Path::new(env!("CARGO_BIN_EXE_aca"))),
            ),
        );
        let (w, h) = (w.to_string(), h.to_string());
        sb.tmux(&["new-session", "-d", "-s", "ui", "-x", &w, "-y", &h, "/bin/sh", script.to_str().unwrap()]);
        let ui = Ui { sb };
        assert!(sb.until(|| ui.screen().contains("快捷键")), "界面没打开");
        // The list ignores keys in the first moment after opening (STARTUP_QUIET)
        std::thread::sleep(std::time::Duration::from_secs(1));
        ui
    }

    fn screen(&self) -> String {
        self.sb.tmux(&["capture-pane", "-p", "-t", "ui"])
    }

    /// One key at a time, as typed by hand (`-l`: as text, not key names)
    fn keys(&self, keys: &[&str]) {
        for k in keys {
            match k.strip_prefix("text:") {
                Some(t) => self.sb.tmux(&["send-keys", "-t", "ui", "-l", t]),
                None => self.sb.tmux(&["send-keys", "-t", "ui", k]),
            };
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    /// Wait until `done` holds and `want` has been on its screen at some point meanwhile: a line in the bottom bar stays only a
    /// few seconds, so it's watched from the start, not looked for once the rest is done
    fn said_while(&self, want: &str, done: impl Fn() -> bool) -> bool {
        let seen = std::cell::Cell::new(false);
        self.sb.until(|| {
            if !seen.get() && self.screen().contains(want) {
                seen.set(true);
            }
            seen.get() && done()
        })
    }

    /// Highlight the session with this title (search, then leave the search: the highlight stays)
    fn pick(&self, title: &str) {
        self.keys(&["/", &format!("text:{title}"), "Enter", "Escape"]);
    }
}

/// On a screen wide enough for the rail: h goes out onto it, j / k go from place to place with the list following, Enter goes
/// into the place and Esc steps back out
#[test]
fn the_rail_goes_from_group_to_group_on_a_wide_screen() {
    let Some(sb) = Sandbox::new("rail") else { return };
    sb.aca(&["new", "fake", &sb.proj(), "-g", "甲组", "-t", "甲一"]);
    sb.aca(&["new", "fake", &sb.proj(), "-g", "乙组", "-t", "乙一"]);
    let ui = Ui::open_sized(&sb, "", 150, 40);
    let s = ui.screen();
    assert!(s.contains("回收站") && s.contains("甲一") && s.contains("乙一"), "{s}");
    // Only one of the two groups' sessions is on screen
    let one = |s: &str| s.contains("甲一") != s.contains("乙一");
    // h: out onto the rail; j: the first group, and the list shows only what is in it
    ui.keys(&["h", "j"]);
    assert!(sb.until(|| one(&ui.screen()) && ui.screen().contains("在组里新建")), "{}", ui.screen());
    let first = ui.screen().contains("甲一");
    ui.keys(&["j"]);
    assert!(sb.until(|| one(&ui.screen()) && ui.screen().contains("甲一") != first), "{}", ui.screen());
    // Enter: into it, the cursor on its session; Esc: back out onto the rail
    ui.keys(&["Enter"]);
    assert!(sb.until(|| ui.screen().contains(&caps("⏎ 打开"))), "{}", ui.screen());
    ui.keys(&["Escape"]);
    assert!(sb.until(|| ui.screen().contains("在组里新建")), "{}", ui.screen());
    // Up to the top row: every group again
    ui.keys(&["k", "k"]);
    assert!(sb.until(|| ui.screen().contains("甲一") && ui.screen().contains("乙一")), "{}", ui.screen());
    // Into the first group, then a: every group with the jump letters showing; a again: back in the group
    ui.keys(&["j", "Enter"]);
    assert!(sb.until(|| one(&ui.screen()) && ui.screen().contains(&caps("a 全部"))), "{}", ui.screen());
    ui.keys(&["a"]);
    let both = |s: &str| s.contains("甲一") && s.contains("乙一");
    assert!(sb.until(|| both(&ui.screen()) && ui.screen().contains(&caps("a 上一处 • 空格 取消"))), "{}", ui.screen());
    ui.keys(&["a"]);
    assert!(sb.until(|| one(&ui.screen()) && ui.screen().contains(&caps("a 全部"))), "{}", ui.screen());
    // Space: the letters show, each group's own capital one before its name on the rail; ⇧ and the other group's goes into
    // that group, the cursor in its list
    ui.keys(&["Space"]);
    assert!(sb.until(|| ui.screen().contains(&caps("⇧字母 进分组"))), "{}", ui.screen());
    let s = ui.screen();
    let (here, other) = if s.contains("甲一") { ("甲组", "乙组") } else { ("乙组", "甲组") };
    let (key, own) = (rail_letter(&s, other), rail_letter(&s, here));
    assert!(key.chars().all(|c| c.is_ascii_uppercase()) && key.len() == 1 && own.len() == 1 && key != own, "{key:?} {own:?}\n{s}");
    ui.keys(&[&format!("text:{key}")]);
    let there = if other == "甲组" { "甲一" } else { "乙一" };
    assert!(sb.until(|| one(&ui.screen()) && ui.screen().contains(there) && ui.screen().contains(&caps("⏎ 打开"))), "{}", ui.screen());
}

/// The capital letter written before a group's name on the rail while the jump letters show ("" = none)
fn rail_letter(screen: &str, group: &str) -> String {
    let rail = |l: &str| l.split('│').next().unwrap_or_default().to_string();
    let Some(row) = screen.lines().map(rail).find(|l| l.trim_end().ends_with(group) || l.contains(&format!("{group} "))) else {
        return String::new();
    };
    let before = &row[..row.find(group).unwrap_or(0)];
    before.trim_end().chars().last().filter(char::is_ascii_uppercase).map(String::from).unwrap_or_default()
}

/// A screen too narrow to keep the rail: `h` lays it over the left, Enter goes into the group under the cursor and puts it
/// away, Esc puts it away and goes back
#[test]
fn the_rail_is_called_out_over_a_narrow_screen() {
    let Some(sb) = Sandbox::new("railnarrow") else { return };
    sb.aca(&["new", "fake", &sb.proj(), "-g", "甲组", "-t", "甲一"]);
    sb.aca(&["new", "fake", &sb.proj(), "-g", "乙组", "-t", "乙一"]);
    let ui = Ui::open(&sb, "");
    let s = ui.screen();
    assert!(!s.contains("回收站") && s.contains("甲一") && s.contains("乙一"), "{s}");
    ui.keys(&["h"]);
    assert!(sb.until(|| ui.screen().contains("回收站") && ui.screen().contains(&caps("⏎ 进入"))), "{}", ui.screen());
    // Onto the first group and in: the rail is gone, the list is that group's
    ui.keys(&["j", "Enter"]);
    let one = |s: &str| s.contains("甲一") != s.contains("乙一");
    assert!(sb.until(|| !ui.screen().contains("回收站") && one(&ui.screen()) && ui.screen().contains(&caps("⏎ 打开"))), "{}", ui.screen());
    let first = ui.screen().contains("甲一");
    // Out again, on to the other group, then Esc: put away, the list back on the first one
    ui.keys(&["h", "j"]);
    // (a group's row: 「写笔记」 (write a note) is its key's word; 「在组里新建」 (new in group) goes first as the bar narrows)
    assert!(sb.until(|| ui.screen().contains("回收站") && ui.screen().contains(&caps("e 写笔记"))), "{}", ui.screen());
    ui.keys(&["Escape"]);
    assert!(sb.until(|| !ui.screen().contains("回收站") && one(&ui.screen()) && ui.screen().contains("甲一") == first), "{}", ui.screen());
    // Tab: the group's notes page, then its tasks page, then back to its sessions; nothing is laid over the screen
    ui.keys(&["Tab"]);
    assert!(sb.until(|| ui.screen().contains("这一组还没有笔记") && !ui.screen().contains("回收站")), "{}", ui.screen());
    ui.keys(&["Tab"]);
    assert!(sb.until(|| ui.screen().contains("这一组还没有任务") && !ui.screen().contains("回收站")), "{}", ui.screen());
    ui.keys(&["Tab"]);
    assert!(sb.until(|| !ui.screen().contains("回收站") && one(&ui.screen())), "{}", ui.screen());
    // h, then Space on the rail laid over: the groups' own capital letters show on it; ⇧ and the other group's goes into
    // that group and puts the rail away
    ui.keys(&["h", "Space"]);
    assert!(sb.until(|| ui.screen().contains("回收站") && ui.screen().contains(&caps("⇧字母 进分组"))), "{}", ui.screen());
    let s = ui.screen();
    let (other, there) = if first { ("乙组", "乙一") } else { ("甲组", "甲一") };
    let key = rail_letter(&s, other);
    assert!(key.len() == 1, "{s}");
    ui.keys(&[&format!("text:{key}")]);
    assert!(sb.until(|| !ui.screen().contains("回收站") && one(&ui.screen()) && ui.screen().contains(there)), "{}", ui.screen());
    // With no rail on screen the letters work unseen from inside a group: Space, then ⇧ and the first group's again
    let back = rail_letter(&s, if first { "甲组" } else { "乙组" });
    ui.keys(&["Space", &format!("text:{back}")]);
    assert!(sb.until(|| one(&ui.screen()) && ui.screen().contains("甲一") == first), "{}", ui.screen());
}

/// `m` with a name no group has: the list says ahead what Enter makes — a group under the one the session is in, working in a
/// directory of that name under that group's — then both are made and the session moved, still running where it was. Tab
/// asks for the directory first
#[test]
fn m_with_a_new_name_makes_a_group_under_this_one_with_a_directory_of_its_own() {
    let Some(sb) = Sandbox::new("movehome") else { return };
    if Command::new("fzf").arg("--version").output().is_err() {
        eprintln!("没有 fzf,跳过");
        return;
    }
    std::fs::create_dir_all(sb.home.join("home")).unwrap();
    let home = sb.home.join("home").canonicalize().unwrap().to_string_lossy().into_owned();
    assert_eq!(sb.aca(&["group", "add", &home]), "home");
    let one = sb.aca(&["new", "fake", &home, "-g", "home", "-t", "调研一"]);
    let two = sb.aca(&["new", "fake", &home, "-g", "home", "-t", "调研二"]);
    let ui = Ui::open_sized(&sb, "", 150, 40);
    let made = |g: &str| sb.run(&["group", "dir", g]).status.success();
    ui.pick("调研一");
    ui.keys(&["m"]);
    assert!(sb.until(|| ui.screen().contains("只换分组")), "{}", ui.screen());
    ui.keys(&["text:rss"]);
    // Above the list's header: the group it makes and where that would work
    let ahead = |s: &str| s.contains("+ home/rss  新分组") && s.contains("/home/rss   Tab 改目录");
    assert!(sb.until(|| ahead(&ui.screen())), "{}", ui.screen());
    ui.keys(&["Enter"]);
    assert!(sb.until(|| made("home/rss")), "{}", ui.screen());
    let dir = format!("{home}/rss");
    assert_eq!(sb.aca(&["group", "dir", "home/rss"]), dir);
    assert!(Path::new(&dir).is_dir());
    let s = sb.json(&["show", &one, "--json"]);
    assert_eq!((s["group"].as_str(), s["dir"].as_str()), (Some("home/rss"), Some(home.as_str())));
    // On the rail it sits under the group it came out of
    let nested = |s: &str| s.contains("    home") && s.contains("      rss");
    assert!(sb.until(|| nested(&ui.screen())), "{}", ui.screen());
    // Tab: its directory is asked first, and the one given instead is the one made
    ui.pick("调研二");
    ui.keys(&["m"]);
    assert!(sb.until(|| ui.screen().contains("只换分组")), "{}", ui.screen());
    ui.keys(&["text:feeds", "Tab"]);
    assert!(sb.until(|| ui.screen().contains("新分组 · home/feeds · 默认目录")), "{}", ui.screen());
    let other = format!("{home}/elsewhere/feeds");
    ui.keys(&["C-u", &format!("text:{other}"), "Enter"]);
    assert!(sb.until(|| made("home/feeds")), "{}", ui.screen());
    assert_eq!(sb.aca(&["group", "dir", "home/feeds"]), other);
    assert!(Path::new(&other).is_dir() && !Path::new(&format!("{home}/feeds")).exists());
    assert_eq!(sb.json(&["show", &two, "--json"])["group"], "home/feeds");
}

/// A scratch directory in config.toml: opening the list makes the directory and the group that works in it, right under all
/// groups on the rail; N with nothing of a project under the cursor starts there; the group can't be removed
#[test]
fn the_scratch_directory_gets_a_group_that_takes_what_is_started_off_any_project() {
    let Some(sb) = Sandbox::new("scratch") else { return };
    let dir = sb.home.join("loose");
    let cfg = sb.home.join("config/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap() + &format!("[scratch]\ndir = \"{}\"\n", dir.display());
    write(&cfg, &text);
    sb.aca(&["new", "fake", &sb.proj(), "-g", "甲组", "-t", "甲一"]);
    let ui = Ui::open_sized(&sb, "", 150, 40);
    assert!(dir.is_dir());
    let real = dir.canonicalize().unwrap().to_string_lossy().into_owned();
    assert_eq!(sb.aca(&["group", "dir", "随手问"]), real);
    let s = ui.screen();
    let at = |what: &str| s.find(what).unwrap_or_else(|| panic!("{what}: {s}"));
    assert!(at("全部") < at("随手问") && at("随手问") < at("甲组"), "{s}");
    // On the rail's row for all groups, N: the box starts in the scratch directory, and Enter starts the session in its group
    ui.keys(&["h", "N"]);
    assert!(sb.until(|| ui.screen().contains("/loose")), "{}", ui.screen());
    ui.keys(&["Enter"]);
    let started = || sb.json(&["ls", "--json"]).as_array().is_some_and(|v| v.iter().any(|x| x["group"] == "随手问" && x["dir"] == real.as_str()));
    assert!(sb.until(started), "{}", sb.aca(&["ls", "--json"]));
    // The printed list goes in the interface's order: the scratch group's sessions before the group made earlier
    let printed = sb.aca(&["ui", "-l"]);
    assert!(printed.find("随手问").zip(printed.find("甲组")).is_some_and(|(s, g)| s < g), "{printed}");
    let o = sb.run(&["group", "rm", "随手问"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("无法删除随手问所在的分组"), "{o:?}");
    assert_eq!(sb.aca(&["group", "dir", "随手问"]), real);
}

/// Several commands started together the very first time all find the registry file new (as every test here does, with a
/// sandbox of its own): each of them opens it
#[test]
fn commands_started_together_on_a_new_registry_all_open_it() {
    let Some(sb) = Sandbox::new("newdb") else { return };
    for round in 0..6 {
        let _ = std::fs::remove_dir_all(sb.home.join("data"));
        let kids: Vec<_> = (0..4)
            .map(|_| {
                sb.cmd().args(["group", "ls"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().unwrap()
            })
            .collect();
        for k in kids {
            let o = k.wait_with_output().unwrap();
            assert!(o.status.success(), "第 {round} 轮:{}", String::from_utf8_lossy(&o.stderr));
        }
    }
}

/// A group's notebook is in the registry: `aca note` and the interface read and write the same notes. The one note an older
/// interface kept in its own file is moved in; a group's notes follow it when it's renamed, go with it when it's deleted and
/// come back with it
#[test]
fn a_groups_notes_are_the_same_ones_on_the_command_line_and_in_the_interface() {
    let Some(sb) = Sandbox::new("notes") else { return };
    let id = sb.aca(&["new", "fake", &sb.proj(), "-g", "甲组", "-t", "甲一"]);
    sb.aca(&["group", "create", "乙组"]);
    assert_eq!(sb.json(&["group", "ls", "--json"]).as_array().map(Vec::len), Some(2));
    // An older interface left the group's one note in its own file: read from the command line, it's a note of yours
    let file = sb.home.join("state/ui/notes");
    write(&file, "{\"group:甲组\": \"旧便签\\n第二行\", \"x1\": \"会话的\"}\n");
    let ls = sb.aca(&["note", "ls", "-g", "甲组"]);
    assert!(ls.contains("旧便签") && ls.contains("你") && !ls.contains("第二行"), "{ls}");
    let left = std::fs::read_to_string(&file).unwrap();
    assert!(!left.contains("group:") && left.contains("会话的"), "{left}");
    // Written from inside a session with no group given: into that session's group, recorded as written by it
    let o = sb.as_session(&id, &["note", "add"], "发布前\n先跑测试\n");
    assert!(o.status.success(), "{o:?}");
    let new = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let notes = sb.json(&["note", "ls", "-g", "甲组", "--json"]);
    let notes = notes.as_array().unwrap();
    assert_eq!(notes.len(), 2, "{notes:?}");
    // The one moved in is pinned (it goes on showing beside the group's row), so it's listed first
    assert_eq!((notes[0]["by"].as_str(), notes[0]["group"].as_str(), notes[0]["pinned"].as_bool()), (Some(""), Some("甲组"), Some(true)));
    assert_eq!(
        (notes[1]["id"].as_str(), notes[1]["by"].as_str(), notes[1]["text"].as_str(), notes[1]["pinned"].as_bool()),
        (Some(new.as_str()), Some(id.as_str()), Some("发布前\n先跑测试"), Some(false))
    );
    // Outside a session a group has to be named, and it has to be one that's there
    let fails = |args: &[&str], why: &str| {
        let o = sb.run(args);
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains(why), "{args:?}:{o:?}");
    };
    fails(&["note", "ls"], "需要指定分组");
    fails(&["note", "add", "-g", "没有的组", "写不进去"], "没有分组");
    // Unpinned and pinned again (the start of its id is enough): where it's listed follows
    let old = notes[0]["id"].as_str().unwrap().to_string();
    sb.aca(&["note", "unpin", &old[..4]]);
    assert!(sb.aca(&["note", "ls", "-g", "甲组"]).starts_with(&new), "{}", sb.aca(&["note", "ls", "-g", "甲组"]));
    sb.aca(&["note", "pin", &old[..4]]);
    assert!(sb.aca(&["note", "ls", "-g", "甲组"]).starts_with(&format!("{old}  ★ 旧便签")), "{}", sb.aca(&["note", "ls", "-g", "甲组"]));
    assert_eq!(sb.aca(&["note", "show", &old]), "旧便签\n第二行");
    // The interface names the pinned note beside the group's rail row
    let ui = Ui::open_sized(&sb, "", 150, 40);
    ui.keys(&["h", "j"]);
    if !ui.screen().contains("旧便签") {
        ui.keys(&["j"]);
    }
    assert!(sb.until(|| ui.screen().contains("★ 旧便签")), "{}", ui.screen());
    // Tab: the group's notes page in the middle column; Enter goes in, the cursor on the first note, the right column showing it
    ui.keys(&["Tab"]);
    assert!(sb.until(|| ui.screen().contains("笔记 2") && ui.screen().contains("发布前")), "{}", ui.screen());
    ui.keys(&["Enter"]);
    assert!(sb.until(|| ui.screen().contains(&caps("e 编辑")) && ui.screen().contains("第二行")), "{}", ui.screen());
    // e rewrites the one under the cursor; n writes a new one (its first line is its name); d and y delete one; ^Z brings it back
    ui.keys(&["e", "text:!", "Escape"]);
    assert!(sb.until(|| sb.aca(&["note", "show", &old]) == "旧便签\n第二行!"), "{}", sb.aca(&["note", "show", &old]));
    ui.keys(&["n", "text:third one", "Escape"]);
    let count = || sb.json(&["note", "ls", "-g", "甲组", "--json"]).as_array().map(Vec::len);
    assert!(sb.until(|| count() == Some(3) && ui.screen().contains("笔记 3")), "{}", ui.screen());
    ui.keys(&["d", "y"]);
    assert!(sb.until(|| count() == Some(2)), "{}", ui.screen());
    ui.keys(&["C-z"]);
    assert!(sb.until(|| count() == Some(3)), "{}", ui.screen());
    ui.keys(&["d", "y"]);
    assert!(sb.until(|| count() == Some(2)), "{}", ui.screen());
    // A deleted note is in the trash (b), like a deleted session: Enter there brings it back, d and y there delete it for good
    ui.keys(&["b"]);
    assert!(sb.until(|| ui.screen().contains("回收站") && ui.screen().contains("✎ third one")), "{}", ui.screen());
    ui.keys(&["Enter"]);
    assert!(ui.said_while("已恢复笔记「third one」", || count() == Some(3)), "{}", ui.screen());
    ui.keys(&["d", "y"]);
    assert!(sb.until(|| count() == Some(2)), "{}", ui.screen());
    ui.keys(&["b"]);
    assert!(sb.until(|| ui.screen().contains("✎ third one")), "{}", ui.screen());
    ui.keys(&["d", "y"]);
    assert!(sb.until(|| ui.screen().contains("回收站为空")), "{}", ui.screen());
    ui.keys(&["b"]);
    assert!(sb.until(|| !ui.screen().contains("回收站为空")), "{}", ui.screen());
    // Tab on to its tasks and round to its sessions; h out onto the group's rail row. The group deleted there: its notes are
    // gone with it, and ^Z brings them back with it
    ui.keys(&["Tab", "Tab"]);
    assert!(sb.until(|| ui.screen().contains(&caps("⏎ 打开"))), "{}", ui.screen());
    ui.keys(&["h"]);
    assert!(sb.until(|| ui.screen().contains("在组里新建")), "{}", ui.screen());
    ui.keys(&["d", "y"]);
    assert!(sb.until(|| !sb.run(&["note", "show", &old]).status.success()), "{}", ui.screen());
    assert!(sb.until(|| !sb.aca(&["group", "ls"]).contains("甲组")), "{}", sb.aca(&["group", "ls"]));
    ui.keys(&["C-z"]);
    assert!(sb.until(|| sb.run(&["note", "show", &new]).status.success()), "{}", ui.screen());
    assert_eq!(sb.json(&["note", "ls", "-g", "甲组", "--json"]).as_array().map(Vec::len), Some(2));
    // Renamed: the notes are the new name's
    sb.aca(&["group", "rename", "甲组", "丙组/甲"]);
    assert_eq!(sb.json(&["note", "show", &old, "--json"])["group"], "丙组/甲");
    // `aca group rm` deletes a group's notes with it; `aca note rm` deletes one
    let other = sb.aca(&["note", "add", "-g", "乙组", "乙组的", "笔记"]);
    assert_eq!(sb.aca(&["note", "show", &other]), "乙组的 笔记");
    sb.aca(&["group", "rm", "乙组"]);
    fails(&["note", "show", &other], "没有笔记");
    assert_eq!(sb.aca(&["note", "rm", &new]), "已删除笔记「发布前」");
    sb.aca(&["note", "unpin", &old]);
    let left = sb.json(&["note", "ls", "-g", "丙组/甲", "--json"]);
    assert_eq!((left.as_array().map(Vec::len), left[0]["pinned"].as_bool()), (Some(1), Some(false)));
    // Both ways of deleting leave the notes in the interface's trash. Selected together and brought back: each is in its
    // notebook again, and the one whose group is gone brings its group back with it
    assert!(sb.until(|| ui.screen().contains("丙组")), "{}", ui.screen());
    ui.keys(&["b"]);
    let both = |s: &str| s.contains("✎ 发布前  丙组/甲") && s.contains("✎ 乙组的 笔记  乙组");
    assert!(sb.until(|| both(&ui.screen())), "{}", ui.screen());
    ui.keys(&["v", "j", "Enter"]);
    let back = || sb.run(&["note", "show", &other]).status.success() && sb.run(&["note", "show", &new]).status.success();
    assert!(ui.said_while("已恢复 2 条", back), "{}", ui.screen());
    assert!(sb.aca(&["group", "ls"]).contains("乙组"), "{}", sb.aca(&["group", "ls"]));
    assert_eq!(sb.json(&["note", "show", &other, "--json"])["group"], "乙组");
}

/// `s` in the list: what's written in the reply box is typed into its input box and Enter pressed, without going in. The agent
/// here reads its input line by line and writes down every line it gets: the text, and the Enter that ends it
#[test]
fn a_reply_written_in_the_list_is_typed_into_the_session_with_its_enter() {
    let Some(sb) = Sandbox::new("reply") else { return };
    let got = sb.home.join("got.txt");
    write(
        &sb.home.join("rd.sh"),
        &format!("#!/bin/sh\necho ready\nwhile IFS= read -r l; do printf '%s\\n' \"$l\" >> '{}'; done\n", got.display()),
    );
    let cfg = sb.home.join("config/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap()
        + &format!("[tools.rd]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", sb.home.join("rd.sh").display());
    write(&cfg, &text);
    let id = sb.aca(&["new", "rd", &sb.proj(), "-t", "回它"]);
    sb.wait_screen(&sb.pane(&id), "ready");
    let ui = Ui::open(&sb, "");
    ui.pick("回它");
    ui.keys(&["s", "text:继续", "Enter"]);
    let lines = || std::fs::read_to_string(&got).unwrap_or_default();
    assert!(ui.said_while("已发给「回它」", || lines() == "继续\n"), "收到的:{:?}\n{}", lines(), ui.screen());
    // Two lines (^J between them): one paste, then one Enter
    ui.keys(&["s", "text:一", "C-j", "text:二", "Enter"]);
    assert!(sb.until(|| lines() == "继续\n一\n二\n"), "收到的:{:?}\n{}", lines(), ui.screen());
}

/// `o` on the rail row of a group that has no router makes one: a session in that group, named after it, told what a router
/// does as it starts, and gone into; back from it the cursor is still on the group's row. `s` there writes to the router;
/// what the router sends out and pulls over shows in the column beside; a stopped one is started by `o`.
/// The agent here writes down the arguments it was started with and every line typed into it, per session
#[test]
fn o_on_a_groups_row_makes_its_router_and_s_there_replies_to_it() {
    let Some(sb) = Sandbox::new("router-ui") else { return };
    let script = sb.home.join("agent.sh");
    add_config(&sb, &format!("[tools.rt]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", script.display()));
    let a = sb.aca(&["new", "rt", &sb.proj(), "-g", "甲组", "-t", "甲一"]);
    let b = sb.aca(&["new", "fake", &sb.proj(), "-g", "乙组", "-t", "乙一"]);
    let ui = Ui::open_sized(&sb, "", 150, 40);
    // What the rail (left of the first │) says on some row
    let rail = |s: &str, want: &str| s.lines().any(|l| l.split('│').next().is_some_and(|cell| cell.contains(want)));
    // Onto 甲组's row on the rail: the list shows only its session
    ui.keys(&["h", "j"]);
    if ui.screen().contains("乙一") {
        ui.keys(&["j"]);
    }
    let on_row = |s: &str| s.contains("甲一") && !s.contains("乙一") && s.contains(&caps("⏎ 进入"));
    assert!(sb.until(|| on_row(&ui.screen()) && ui.screen().contains(&caps("o 新建 router"))), "{}", ui.screen());
    assert!(!rail(&ui.screen(), "@"), "{}", ui.screen());
    // Nobody to reply to yet
    ui.keys(&["s"]);
    assert!(sb.until(|| ui.screen().contains("还没有 router · o 新建")), "{}", ui.screen());
    // o: the new-session box, for a router; ⏎ makes it and goes into it
    ui.keys(&["o"]);
    assert!(sb.until(|| ui.screen().contains("新建 router") && ui.screen().contains("⏎ 新建")), "{}", ui.screen());
    ui.keys(&["Enter"]);
    let named = || sb.run(&["group", "router", "甲组"]);
    assert!(sb.until(|| named().status.success()), "{}", ui.screen());
    let r = String::from_utf8_lossy(&named().stdout).trim().to_string();
    let shown = sb.json(&["show", &r, "--json"]);
    assert_eq!((shown["title"].as_str(), shown["group"].as_str(), shown["tool"].as_str()), (Some("甲组 router"), Some("甲组"), Some("rt")));
    let args = || String::from_utf8_lossy(&std::fs::read(sb.home.join(format!("args-{r}"))).unwrap_or_default()).into_owned();
    let opening = || std::fs::read_to_string(sb.home.join(format!("open-{r}"))).unwrap_or_default();
    assert!(
        sb.until(|| args().contains("aca-router:router\0") && opening().contains("项目交接 · 甲组") && opening().contains(&a)),
        "{}",
        opening()
    );
    assert!(opening().ends_with("看完等用户说话,只回一个字。"));
    // The terminal is the router's now
    assert!(sb.until(|| !ui.screen().contains("快捷键") && ui.screen().contains("ready")), "{}", ui.screen());
    // Out again, as Ctrl+Q does: still on the group's row, which leads with @ now, and the bar has the router's keys
    sb.tmux(&["detach-client", "-s", "=甲组 router"]);
    let with_router = |s: &str| on_row(s) && rail(s, "@ 甲组") && s.contains(&caps("s 回话 • o 打开 router"));
    assert!(sb.until(|| with_router(&ui.screen())), "{}", ui.screen());
    // What the router does is in the column beside: what it hands a session, a session it pulls over (its row joins the list)
    assert!(sb.as_session(&r, &["msg", &a, "看一下登录"], "").status.success());
    assert!(sb.as_session(&r, &["move", "--tree", &b, "甲组"], "").status.success());
    assert!(sb.until(|| ui.screen().contains("@ → 甲一") && ui.screen().contains("@ ← 乙组")), "{}", ui.screen());
    // s: written on the line under the thread; ⏎ types it into the router, and you stay on the row
    ui.keys(&["s", "text:先看登录"]);
    assert!(sb.until(|| ui.screen().contains("❯ 先看登录") && ui.screen().contains(&caps("⏎ 发送"))), "{}", ui.screen());
    ui.keys(&["Enter"]);
    let got = || std::fs::read_to_string(sb.home.join(format!("got-{r}"))).unwrap_or_default();
    assert!(ui.said_while("已发给「甲组 router」", || got() == "先看登录\n"), "收到的:{:?}\n{}", got(), ui.screen());
    // Its agent starts a turn with it: the thread has it as what you said
    assert!(
        sb.until(|| {
            let screen = ui.screen();
            screen.contains(&caps("⏎ 进入")) && !screen.contains(&caps("⏎ 发送")) && screen.matches("❯ 先看登录").count() == 1
        }),
        "{}",
        ui.screen()
    );
    let pane = sb.pane(&r);
    sb.hook(&r, &pane, r#"{"hook_event_name":"UserPromptSubmit","session_id":"c1","prompt":"先看登录"}"#);
    assert!(session_events(&sb, &r, "turn.started").iter().any(|e| e["prompt"] == "先看登录"));
    // Seeing the reply proves the thread has refreshed past the submit.
    sb.hook(&r, &pane, r#"{"hook_event_name":"Stop","session_id":"c1","last_assistant_message":"收到登录请求"}"#);
    assert!(sb.until(|| ui.screen().contains("收到登录请求")), "{}", ui.screen());
    let screen = ui.screen();
    assert_eq!(screen.matches("❯ 先看登录").count(), 1, "{screen}");
    // A message starts the next shift; o then goes into it.
    sb.aca(&["stop", &r]);
    assert!(sb.until(|| ui.screen().contains(&caps("o 启动 router"))), "{}", ui.screen());
    ui.keys(&["s"]);
    assert!(sb.until(|| ui.screen().contains(&caps("⏎ 发送"))), "{}", ui.screen());
    ui.keys(&["text:换班查一下", "Enter"]);
    assert!(sb.until(|| opening().contains("叫醒你的") && opening().contains("换班查一下")), "{}", opening());
    assert!(sb.until(|| ui.screen().contains(&caps("s 回话"))), "{}", ui.screen());
    ui.keys(&["o"]);
    assert!(sb.until(|| !ui.screen().contains("快捷键") && ui.screen().contains("ready")), "{}", ui.screen());
    assert_ne!(sb.json(&["show", &r, "--json"])["state"], "stopped");
    sb.tmux(&["detach-client", "-s", "=甲组 router"]);
    assert!(sb.until(|| ui.screen().contains(&caps("o 打开 router")) && ui.screen().contains(&caps("⏎ 进入"))), "{}", ui.screen());
}

/// `o` on the rail's row for all groups makes the desk. With no scratch directory set that is asked for first; the session
/// is in the scratch group, named after the desk, told what the desk does as it starts, and gone into; back from it the
/// cursor is still on the row for all groups, which leads with @ now. `s` there writes to the desk; what it hands out shows
/// in the column beside; a stopped one is started by `o`.
/// The agent here writes down the arguments it was started with and every line typed into it, per session
#[test]
fn o_on_the_row_for_all_groups_makes_the_desk_and_s_there_replies_to_it() {
    let Some(sb) = Sandbox::new("desk-ui") else { return };
    let script = sb.home.join("agent.sh");
    add_config(&sb, &format!("[tools.rt]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", script.display()));
    let a = sb.aca(&["new", "rt", &sb.proj(), "-g", "甲组", "-t", "甲一"]);
    let ui = Ui::open_sized(&sb, "", 150, 40);
    // What the rail (left of the first │) says on some row
    let rail = |s: &str, want: &str| s.lines().any(|l| l.split('│').next().is_some_and(|cell| cell.contains(want)));
    // Onto the row for all groups: no desk yet, and nobody to reply to — from the command line either
    ui.keys(&["h"]);
    assert!(sb.until(|| ui.screen().contains(&caps("⏎ 进入 • o 新建总台"))), "{}", ui.screen());
    assert!(!rail(&ui.screen(), "@"), "{}", ui.screen());
    let nobody = sb.run(&["msg", "desk", "你好"]);
    assert_eq!(nobody.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&nobody.stderr).contains("还没有总台"), "{nobody:?}");
    ui.keys(&["s"]);
    assert!(sb.until(|| ui.screen().contains("还没有总台 · o 新建")), "{}", ui.screen());
    // o: there is no scratch directory for a desk to be in, so that is asked for first
    ui.keys(&["o"]);
    assert!(sb.until(|| ui.screen().contains("随手问目录")), "{}", ui.screen());
    let dir = sb.home.join("asks");
    ui.keys(&[&format!("text:{}", dir.display()), "Enter"]);
    // Given: written down, its group made, and on to the desk's box — in that group and directory, with the tool of the
    // row the list is on
    assert!(sb.until(|| ui.screen().contains("新建总台") && ui.screen().contains("⏎ 新建")), "{}", ui.screen());
    let s = ui.screen();
    assert!(s.contains("分组   随手问") && s.contains("/asks") && s.contains("rt"), "{s}");
    let cfg = std::fs::read_to_string(sb.home.join("config/config.toml")).unwrap();
    assert!(cfg.contains("[scratch]") && cfg.contains(&format!("dir = \"{}\"", dir.display())), "{cfg}");
    assert!(dir.is_dir());
    ui.keys(&["Enter"]);
    let named = || sb.run(&["group", "router", "随手问"]);
    assert!(sb.until(|| named().status.success()), "{}", ui.screen());
    let r = String::from_utf8_lossy(&named().stdout).trim().to_string();
    let shown = sb.json(&["show", &r, "--json"]);
    assert_eq!((shown["title"].as_str(), shown["group"].as_str(), shown["tool"].as_str()), (Some("总台"), Some("随手问"), Some("rt")));
    let args = || String::from_utf8_lossy(&std::fs::read(sb.home.join(format!("args-{r}"))).unwrap_or_default()).into_owned();
    let opening = || std::fs::read_to_string(sb.home.join(format!("open-{r}"))).unwrap_or_default();
    let told = || args().contains("aca-router:desk\0") && opening().contains("总台交接 · 项目") && opening().contains("项目交接 · 随手问");
    assert!(sb.until(told), "{}", args());
    // The terminal is the desk's now
    assert!(sb.until(|| !ui.screen().contains("快捷键") && ui.screen().contains("ready")), "{}", ui.screen());
    // Out again, as Ctrl+Q does: still on the row for all groups, which leads with @ now (the scratch group's name doesn't),
    // and the bar has the desk's keys
    sb.tmux(&["detach-client", "-s", "=总台"]);
    let with_desk = |s: &str| rail(s, "@ 全部") && !rail(s, "@ 随手问") && s.contains(&caps("⏎ 进入 • s 回话 • o 打开总台"));
    assert!(sb.until(|| with_desk(&ui.screen())), "{}", ui.screen());
    // What the desk hands a session is in the column beside
    assert!(sb.as_session(&r, &["msg", &a, "看一下登录"], "").status.success());
    assert!(sb.until(|| ui.screen().contains("@ → 甲一")), "{}", ui.screen());
    // A session it starts the way it is told to is yours: nobody's child, in the group named, and the event says whose
    // command made it — so the column still has it as one the desk started
    let o = sb.as_session(&r, &["new", "rt", &sb.proj(), "--parent", "-", "-g", "甲组", "-t", "查登录", "--hold"], "");
    assert!(o.status.success(), "{o:?}");
    let made = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let shown = sb.json(&["show", &made, "--json"]);
    assert_eq!((shown["parent"].as_str().unwrap_or(""), shown["group"].as_str()), ("", Some("甲组")));
    let events: Vec<Value> = sb.aca(&["events", "-n", "50"]).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let created = events.iter().find(|e| e["event"] == "session.created" && e["session"] == made.as_str()).unwrap();
    assert_eq!((created["by"].as_str(), created["parent"].as_str()), (Some(r.as_str()), Some("")));
    assert!(sb.until(|| ui.screen().lines().any(|l| l.contains("@ → 查登录") && l.contains("新会话"))), "{}", ui.screen());
    // The scratch group's own row shows the same exchange, with the same keys
    ui.keys(&["j"]);
    assert!(sb.until(|| ui.screen().contains("@ → 甲一") && ui.screen().contains(&caps("o 打开总台 • Tab 换页"))), "{}", ui.screen());
    ui.keys(&["k"]);
    // What comes in from outside goes to `desk`, whichever session that is, and says which channel it came through: kept
    // on the message, and written at the right end of its line
    let put = sb.aca(&["msg", "desk", "--via", "Discord", "帮我看下周的机票"]);
    assert!(put.contains(&r), "{put}");
    let sent: Vec<Value> = sb.aca(&["events", "-n", "50"]).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let relayed = sent.iter().find(|e| e["event"] == "message.sent" && e["text"] == "帮我看下周的机票").unwrap();
    assert_eq!((relayed["session"].as_str(), relayed["from"].as_str(), relayed["via"].as_str()), (Some(r.as_str()), Some("you"), Some("Discord")));
    let named = |s: &str| s.lines().any(|l| l.contains("❯ 帮我看下周的机票") && l.contains("Discord"));
    assert!(sb.until(|| named(&ui.screen())), "{}", ui.screen());
    assert!(sb.aca(&["inbox", &r]).contains("说(Discord)"), "{}", sb.aca(&["inbox", &r]));
    // A channel is where your words came from: a session can't name one, and the name is a short word
    let o = sb.as_session(&r, &["msg", &a, "--via", "Discord", "冒充"], "");
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--via 只能在会话外使用"), "{o:?}");
    for bad in ["claude", "two words", "averyveryverylongname"] {
        assert!(!sb.run(&["msg", "desk", "--via", bad, "x"]).status.success(), "{bad}");
    }
    // s: written on the line under the thread; ⏎ types it into the desk, and you stay on the row
    ui.keys(&["s", "text:电压的事给谁"]);
    assert!(sb.until(|| ui.screen().contains("❯ 电压的事给谁") && ui.screen().contains(&caps("⏎ 发送"))), "{}", ui.screen());
    ui.keys(&["Enter"]);
    let got = || std::fs::read_to_string(sb.home.join(format!("got-{r}"))).unwrap_or_default();
    // (what was relayed above may have been typed in before it, or still be waiting in its inbox)
    assert!(ui.said_while("已发给「总台」", || got().lines().last() == Some("电压的事给谁")), "收到的:{:?}\n{}", got(), ui.screen());
    // A message starts the next shift; o then goes into it.
    sb.aca(&["stop", &r]);
    assert!(sb.until(|| ui.screen().contains(&caps("o 启动总台"))), "{}", ui.screen());
    ui.keys(&["s"]);
    assert!(sb.until(|| ui.screen().contains(&caps("⏎ 发送"))), "{}", ui.screen());
    ui.keys(&["text:换班查一下", "Enter"]);
    assert!(sb.until(|| opening().contains("叫醒你的") && opening().contains("换班查一下")), "{}", opening());
    assert!(sb.until(|| ui.screen().contains(&caps("s 回话"))), "{}", ui.screen());
    ui.keys(&["o"]);
    assert!(sb.until(|| !ui.screen().contains("快捷键") && ui.screen().contains("ready")), "{}", ui.screen());
    assert_ne!(sb.json(&["show", &r, "--json"])["state"], "stopped");
    sb.tmux(&["detach-client", "-s", "=总台"]);
    assert!(sb.until(|| ui.screen().contains(&caps("o 打开总台")) && ui.screen().contains(&caps("⏎ 进入"))), "{}", ui.screen());
}

/// `s` on a row waiting for you: a single choice is answered by its number, a startup gate by walking to its option — from
/// the list, by you. The list here was opened carrying another session's ACA_SESSION_ID (as a popup over that session may):
/// that session couldn't answer these with `aca answer`, the person at the list can, and it's recorded as you
#[test]
fn a_question_is_answered_from_the_list_with_a_key_and_counted_as_yours() {
    let Some(sb) = Sandbox::new("answer-ui") else { return };
    sb.screen_tools();
    let other = sb.aca(&["new", "fake", &sb.proj(), "-t", "别的"]);
    let w = sb.aca(&["new", "scr", &sb.proj(), "-t", "选择题"]);
    let pane = sb.pane(&w);
    sb.set_screen(&w, &pane, " ☐ 测试选择\n测试选 A 还是 B？\n❯ 1. A\n  2. B\nEnter to select · ↑/↓ to navigate · Esc to cancel");
    sb.hook(
        &w,
        &pane,
        r#"{"hook_event_name":"PreToolUse","session_id":"c1","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"测试选 A 还是 B？","multiSelect":false,"options":[{"label":"A"},{"label":"B"}]}]}}"#,
    );
    let g = sb.aca(&["new", "fcx", &sb.proj(), "-t", "信任框"]);
    let gpane = sb.pane(&g);
    sb.set_screen(
        &g,
        &gpane,
        "Folder access\n  /p/api\n  Trust this folder? Codex can read, edit, and run files here.\n› 1. Trust and continue\n  2. Quit\n  enter continue · esc quit",
    );
    // The session the list's environment names can't answer them itself
    let o = sb.as_session(&other, &["answer", &w, "2"], "");
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("不是你派出的会话"));

    let ui = Ui::open(&sb, &format!("ACA_SESSION_ID={other}"));
    ui.pick("选择题");
    ui.keys(&["s"]);
    assert!(sb.until(|| ui.screen().contains("1–2 选")), "{}", ui.screen());
    let shown = ui.screen();
    assert!(shown.contains("回答 · 选择题") && shown.contains("测试选 A 还是 B？") && shown.contains("2 B"), "{shown}");
    ui.keys(&["2"]);
    // The fake agent doesn't read keys: the terminal echoes the 2 on its screen
    let echoed = || sb.tmux(&["capture-pane", "-p", "-t", &pane]).ends_with("Esc to cancel\n2");
    assert!(ui.said_while("已回答「选择题」:2 B", echoed), "{}\n{}", sb.tmux(&["capture-pane", "-p", "-t", &pane]), ui.screen());
    let answered = sb.aca(&["events", "-n", "100"]).lines().find(|l| l.contains("ask.answered")).unwrap().to_string();
    assert!(answered.contains("\"by\":\"you\"") && answered.contains("\"answer\":\"2 B\""), "{answered}");

    // The gate the list read off its screen is the one answering reads: one down, then Enter (echoed as ^[[B)
    ui.pick("信任框");
    ui.keys(&["s"]);
    assert!(sb.until(|| ui.screen().contains("2 Quit")), "{}", ui.screen());
    ui.keys(&["2"]);
    let screen = sb.wait_screen(&gpane, "^[[B");
    assert!(screen.contains("esc quit\n^[[B"), "{screen}");
    assert!(sb.until(|| sb.aca(&["events", "-n", "100"]).contains("\"kind\":\"gate\"")), "{}", ui.screen());
}

/// A window opened inside a session with a built-in agent in it (an agent team's teammate): `s` on that window's row types
/// into the window's own pane, not the session's. Both write down every line they get: the session's pane runs the same
/// line reader as above, the window a compiled program called claude (the name is what makes it a built-in agent, see
/// `foreign_claude`)
#[test]
fn a_reply_to_a_window_goes_into_that_pane_and_not_the_sessions_own() {
    let Some(sb) = Sandbox::new("reply-win") else { return };
    let (main, win) = (sb.home.join("main.txt"), sb.home.join("win.txt"));
    let bin = sb.home.join("bin/claude");
    let src = sb.home.join("bin/rd.rs");
    write(
        &src,
        "use std::io::{BufRead, Write};\nfn main() {\n    let path = std::env::args().nth(1).unwrap();\n    for l in std::io::stdin().lock().lines() {\n        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();\n        writeln!(f, \"{}\", l.unwrap()).unwrap();\n    }\n}\n",
    );
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    if !Command::new(rustc).arg("-o").arg(&bin).arg(&src).status().is_ok_and(|s| s.success()) {
        eprintln!("编不出假的 claude,跳过");
        return;
    }
    write(
        &sb.home.join("rd.sh"),
        &format!("#!/bin/sh\necho ready\nwhile IFS= read -r l; do printf '%s\\n' \"$l\" >> '{}'; done\n", main.display()),
    );
    let cfg = sb.home.join("config/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap()
        + &format!("[tools.rd]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", sb.home.join("rd.sh").display());
    write(&cfg, &text);
    let id = sb.aca(&["new", "rd", &sb.proj(), "-t", "主会话"]);
    let pane = sb.pane(&id);
    sb.wait_screen(&pane, "ready");
    let session = sb.tmux(&["display-message", "-p", "-t", &pane, "#{session_id}"]);
    let wpane = sb.tmux(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &format!("{session}:"),
        &format!("'{}' '{}'", bin.display(), win.display()),
    ]);
    assert!(wpane.starts_with('%'), "{wpane}");
    let ui = Ui::open(&sb, "");
    assert!(sb.until(|| ui.screen().lines().any(|l| l.contains("└ ○ #") && l.contains("claude"))), "{}", ui.screen());
    ui.pick("主会话");
    ui.keys(&["Down", "s", "text:进第二格", "Enter"]);
    let got = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    // The window's title comes from the command tmux names for its pane, which on a busy machine is now and then some other
    // process's (kernel_task, seen): only that it was sent is checked here, where it went is checked by what each pane got
    assert!(ui.said_while("已发给「", || got(&win) == "进第二格\n"), "窗口收到的:{:?}\n{}", got(&win), ui.screen());
    assert_eq!(got(&main), "", "主窗格不该收到");
}

fn router_tool(sb: &Sandbox, router: &str, tool: &str, args: Value) -> String {
    let req = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":format!("aca-router__{tool}"),"arguments":args}});
    let out = sb.as_session(router, &["plugin", "tools"], &format!("{req}\n"));
    assert!(out.status.success(), "{out:?}");
    let reply = serde_json::from_slice::<Value>(&out.stdout).unwrap();
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    reply["result"]["content"][0]["text"].as_str().unwrap().trim_end_matches('\n').into()
}

fn leave_draft(sb: &Sandbox, id: &str) {
    let pane = sb.pane(id);
    sb.tmux(&["set-buffer", "-b", "draft", "我的草稿"]);
    sb.tmux(&["paste-buffer", "-p", "-d", "-b", "draft", "-t", &pane]);
    assert!(sb.wait_screen(&pane, "❯ 我的草稿").contains("❯ 我的草稿"));
}

fn bell_worker(sb: &Sandbox) -> String {
    write(&sb.home.join("follow-bell"), "");
    add_config(sb, &format!("[tools.mail]\nkind='claude'\ncmd=['python3',{:?}]\n", sb.home.join("router.py")));
    sb.aca(&["new", "mail", &sb.proj(), "-g", "project", "-t", "worker", "--hold"])
}

fn session_events(sb: &Sandbox, id: &str, name: &str) -> Vec<Value> {
    sb.aca(&["events", "-n", "100"])
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|e| e["session"] == id && e["event"] == name)
        .collect()
}

fn delivery_methods(sb: &Sandbox, id: &str) -> Vec<Value> {
    session_events(sb, id, "message.delivered").into_iter().map(|e| e["how"].clone()).collect()
}

#[test]
fn connected_claude_rings_the_bell_when_pasting_fails() {
    use std::os::unix::fs::PermissionsExt;
    let Some(sb) = Sandbox::new("paste-error") else { return };
    let w = bell_worker(&sb);
    sb.aca(&["start", &w]);
    assert!(sb.wait_screen(&sb.pane(&w), "ready").contains("ready"));
    let bin = sb.home.join("bin");
    let shim = bin.join("tmux");
    write(
        &shim,
        r#"#!/bin/sh
if [ "$3" = paste-buffer ]; then
    touch "$ACA_HOME/paste-failed"
    echo "injected paste failure" >&2
    exit 1
fi
PATH="$TEST_TMUX_PATH" exec tmux "$@"
"#,
    );
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var("PATH").unwrap();
    let mut cmd = sb.cmd();
    cmd.env("PATH", format!("{}:{path}", bin.display())).env("TEST_TMUX_PATH", path);
    let out = cmd.args(["msg", &w, "可以"]).output().unwrap();
    assert!(sb.home.join("paste-failed").exists());
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("已按门铃"));
    assert!(sb.until(|| std::fs::read_to_string(sb.home.join("bell-submit")).is_ok_and(|s| s.contains("「可以」"))));
    assert_eq!(delivery_methods(&sb, &w), ["bell"]);
}

#[test]
fn connected_claude_types_when_idle_and_rings_while_busy_or_with_a_draft() {
    let Some(sb) = Sandbox::new("type-or-bell") else { return };
    let w = bell_worker(&sb);
    sb.aca(&["start", &w]);
    let pane = sb.pane(&w);
    assert!(sb.wait_screen(&pane, "ready").contains("ready"));
    let bell = sb.home.join(format!("state/inbox/{w}.bell"));
    let before = std::fs::read(&bell).unwrap();
    let send = || sb.aca(&["msg", &w, "可以"]);
    let submits = || session_events(&sb, &w, "turn.started");
    assert!(send().contains("已输入并发送"));
    assert!(sb.until(|| submits().iter().any(|e| e["prompt"] == "可以")));
    assert_eq!(delivery_methods(&sb, &w), ["type"]);
    assert_eq!(std::fs::read(bell).unwrap(), before);

    assert!(send().contains("已按门铃"));
    assert!(sb.until(|| submits().len() == 2));
    sb.hook(&w, &pane, r#"{"hook_event_name":"Stop","last_assistant_message":"完成"}"#);
    leave_draft(&sb, &w);
    assert!(send().contains("已按门铃"));
    assert!(sb.until(|| submits().len() == 3));
    assert!(submits()[1..].iter().all(|e| e["prompt"].as_str().unwrap().starts_with("<task-notification>")));
    assert_eq!(delivery_methods(&sb, &w), ["type", "bell", "bell"]);
    assert!(sb.tmux(&["capture-pane", "-p", "-t", &pane]).contains("❯ 我的草稿"));
    // Submit the draft still in the fake's input buffer, checking its actual contents.
    sb.tmux(&["send-keys", "-t", &pane, "Enter"]);
    assert!(sb.until(|| submits().last().is_some_and(|e| e["prompt"] == "我的草稿")));
    assert_eq!(std::fs::read_to_string(sb.home.join(format!("got-{w}"))).unwrap(), "可以\n我的草稿\n");

    sb.hook(&w, &pane, r#"{"hook_event_name":"Stop","last_assistant_message":"好"}"#);
    let long = format!("  第一行\n{}  ", "字".repeat(4100));
    sb.aca(&["msg", &w, &long]);
    assert!(sb.until(|| std::fs::read_to_string(sb.home.join(format!("got-{w}"))).is_ok_and(|s| s.ends_with(&format!("{long}\n")))));
}

#[test]
fn connected_claude_leaves_peer_mail_in_a_mixed_batch_until_stop() {
    let Some(sb) = Sandbox::new("mixed-bell") else { return };
    let w = bell_worker(&sb);
    let peer = sb.aca(&["new", "fake", &sb.proj(), "--hold"]);
    assert!(sb.as_session(&peer, &["msg", &w, "同事写的"], "").status.success());
    sb.aca(&["msg", &w, "可以"]);
    sb.aca(&["start", &w]);
    assert!(sb.wait_screen(&sb.pane(&w), "ready").contains("ready"));
    let bell_path = sb.home.join(format!("state/inbox/{w}.bell"));
    let before = std::fs::read(&bell_path).unwrap();
    assert!(sb.until(|| session_events(&sb, &w, "turn.started").iter().any(|e| e["prompt"] == "可以")));
    let mail = sb.json(&["inbox", &w, "--json"]);
    assert_eq!(mail[0]["stage"], "pending");
    assert_eq!(mail[1]["stage"], "got");
    assert_eq!(delivery_methods(&sb, &w), ["type"]);
    assert_eq!(std::fs::read(&bell_path).unwrap(), before);
    sb.hook(&w, "", r#"{"hook_event_name":"Stop","last_assistant_message":"完成"}"#);
    assert!(sb.until(|| sb.home.join("bell-submit").is_file()));
    assert_eq!(session_events(&sb, &w, "turn.started").len(), 2);
    let deliveries = session_events(&sb, &w, "message.delivered");
    assert_eq!(deliveries.len(), 2);
    assert_eq!((&deliveries[0]["id"], &deliveries[1]["id"]), (&mail[1]["id"], &mail[0]["id"]));
    assert_eq!(deliveries[1]["how"], "bell");
    assert_ne!(std::fs::read(&bell_path).unwrap(), before);
    let bell = std::fs::read_to_string(sb.home.join("bell-submit")).unwrap();
    assert!(bell.contains("同事写的") && !bell.contains("可以"), "{bell}");
    assert_eq!(std::fs::read_to_string(sb.home.join(format!("got-{w}"))).unwrap(), "可以\n");
}

#[test]
fn connected_claude_receives_interface_replies_as_user_mail() {
    let Some(sb) = Sandbox::new("ui-type-or-bell") else { return };
    let w = bell_worker(&sb);
    sb.aca(&["start", &w]);
    let pane = sb.pane(&w);
    assert!(sb.wait_screen(&pane, "ready").contains("ready"));
    let ui = Ui::open(&sb, "");
    ui.pick("worker");
    ui.keys(&["s", "text:可以", "Enter"]);
    assert!(sb.until(|| session_events(&sb, &w, "turn.started").iter().any(|e| e["prompt"] == "可以")));
    assert_eq!(delivery_methods(&sb, &w), ["type"]);
    ui.keys(&["s", "text:再说一句"]);
    sb.tmux(&["select-pane", "-T", "⠋ Working", "-t", &pane]);
    ui.keys(&["Enter"]);
    assert!(sb.until(|| std::fs::read_to_string(sb.home.join("bell-submit")).is_ok_and(|s| s.contains("再说一句"))));
    let mail = sb.json(&["inbox", &w, "--json"]);
    assert!(mail.as_array().unwrap().iter().all(|m| m["from"] == "you" && m["kind"] == "msg" && m["at"] == 0 && m["what"] == ""));
    assert_eq!(delivery_methods(&sb, &w), ["type", "bell"]);
}

#[test]
fn connected_claude_router_types_channel_mail_and_only_its_first_quote_to_a_worker() {
    use serde_json::json;
    let Some(sb) = Sandbox::new("typed-dispatch") else { return };
    let r = shift_fixture(&sb);
    std::fs::remove_file(sb.home.join("manual-submit")).unwrap();
    let w = bell_worker(&sb);
    sb.aca(&["start", &w]);
    assert!(sb.wait_screen(&sb.pane(&w), "ready").contains("ready"));
    sb.aca(&["start", &r]);
    assert!(sb.until(|| !session_events(&sb, &r, "turn.done").is_empty()));
    let words = "可以，保留这句";
    sb.aca(&["msg", &r, "--via", "Discord", words]);
    assert!(sb.until(|| session_events(&sb, &r, "turn.started").iter().any(|e| e["prompt"] == words)));
    let send = |text| router_tool(&sb, &r, "message", json!({"to": w, "text": text}));
    send("请继续");
    assert!(sb.until(|| !session_events(&sb, &w, "turn.started").is_empty()));
    let typed = std::fs::read_to_string(sb.home.join(format!("got-{w}"))).unwrap();
    assert!(typed.starts_with(&format!("[aca] 来自会话 {r}")) && typed.contains("用户对「dispatcher」说(Discord,"), "{typed}");
    assert!(typed.ends_with(&format!("「{words}」\ndispatcher:请继续\n")), "{typed}");
    // An idle recipient could accept another paste; this turn's second message still takes the bell.
    sb.hook(&w, "", r#"{"hook_event_name":"Stop","last_assistant_message":"收到"}"#);
    send("第二句");
    assert!(sb.until(|| std::fs::read_to_string(sb.home.join("bell-submit")).is_ok_and(|s| s.contains("「第二句」"))));
    let mail = sb.json(&["inbox", &w, "--json"]);
    assert_eq!(mail[0]["quoted"], true);
    assert!(mail[1].get("quoted").is_none());
    assert_eq!(mail[1]["text"], "第二句");
    assert_eq!(delivery_methods(&sb, &w), ["type", "bell"]);
    assert_eq!(delivery_methods(&sb, &r), ["type"]);
    assert_eq!(std::fs::read_to_string(sb.home.join(format!("got-{r}"))).unwrap(), format!("{words}\n"));
    sb.hook(&r, "", r#"{"hook_event_name":"Stop","last_assistant_message":"已转交"}"#);
    assert_eq!(session_events(&sb, &r, "turn.done").last().unwrap()["via"], "Discord");
}

#[test]
fn a_router_dispatches_the_opening_users_words_through_all_four_tools() {
    use serde_json::json;
    let Some(sb) = Sandbox::new("router-words") else { return };
    // This test drives each submit itself, including the first one after startup.
    write(&sb.home.join("manual-submit"), "");
    let dir = sb.proj();
    sb.aca(&["group", "create", "p", &dir]);
    let new = |title| sb.aca(&["new", "fake", &dir, "-g", "p", "-t", title, "--hold"]);
    let (r, w) = (new("aps"), new("worker"));
    sb.aca(&["group", "router", "p", &r]);
    sb.aca(&["start", &r]);
    sb.aca(&["plugin", "install", "aca.router"]);
    let run = |tool, args| router_tool(&sb, &r, tool, args);
    let last = || sb.json(&["inbox", &w, "--json"]).as_array().unwrap().last().unwrap()["text"].as_str().unwrap().to_string();
    let prompt = |words| sb.hook(&r, "", &json!({"hook_event_name":"UserPromptSubmit","prompt":words}).to_string());
    let words = "  可以,,你帮我决定吧\n ";
    prompt(words);
    run("ticket", json!({"title":"fix","criteria":"OK","to":w}));
    assert!(last().contains(&format!("「{words}」\naps:任务 #1:fix")), "{}", last());
    run("message", json!({"to":w,"text":" 再一句 "}));
    assert_eq!(last(), " 再一句 ");
    sb.aca(&["msg", "--via", "Discord", &r, words]);
    assert_eq!(sb.as_session(&r, &["inbox", "take"], "").status.code(), Some(2));
    run("give", json!({"ticket":1,"to":w,"say":"  follow  "}));
    assert!(last().starts_with("用户对「aps」说(Discord,") && last().contains(&format!("「{words}」\naps:任务 #1:fix")), "{}", last());
    write(&sb.home.join("agent.sh"), "#!/bin/sh\nprintf '%s' \"$@\" > \"$ACA_HOME/open-$ACA_SESSION_ID\"\nexec sleep 600\n");
    let opened = run("start", json!({"title":"second","say":" go ","tool":"fake"}));
    let id = opened.strip_prefix("#2 ").unwrap();
    assert!(sb.until(|| {
        std::fs::read_to_string(sb.home.join(format!("open-{id}")))
            .is_ok_and(|s| s.contains("用户对「aps」说(Discord,") && s.contains(&format!("「{words}」\naps:任务 #2:second")))
    }));
    assert!(sb.as_session(&w, &["msg", &r, "worker words"], "").status.success());
    assert_eq!(sb.as_session(&r, &["inbox", "take"], "").status.code(), Some(2));
    run("message", json!({"to":w,"text":"  follow  "}));
    assert_eq!(last(), "  follow  ");
    prompt("再问一次");
    let text = "路".repeat(4000);
    run("message", json!({"to":w,"text":text}));
    assert!(last().ends_with(&format!("「再问一次」\naps:{text}")));
}

#[test]
fn doorbell_words_reach_four_distinct_router_tool_recipients() {
    let Some(sb) = Sandbox::new("bell-dispatch") else { return };
    let r = shift_fixture(&sb);
    std::fs::remove_file(sb.home.join("manual-submit")).unwrap();
    write(&sb.home.join("follow-bell"), "");
    let new = |name| sb.aca(&["new", "fake", &sb.proj(), "-g", "project", "-t", name, "--hold"]);
    let (a, b, c) = (new("message"), new("ticket"), new("give"));
    sb.aca(&["start", &r]);
    assert!(sb.until(|| sb.home.join("watching").exists() && sb.event_names().iter().any(|e| e == "turn.done")));
    leave_draft(&sb, &r);
    let words = "可以,你帮我决定吧\n保留这句 `原话`。";
    sb.aca(&["msg", "--via", "Discord", &r, words]);
    assert!(sb.until(|| std::fs::read_to_string(sb.home.join("bell-submit")).is_ok_and(|s| s.contains("<task-notification>"))));
    for text in four_tool_deliveries(&sb, &r, [a, b, c]) {
        assert!(text.starts_with("用户对「dispatcher」说(Discord,") && text.contains(&format!("):\n「{words}」\ndispatcher:")), "{text}");
    }
}

#[test]
fn wrapped_user_words_reach_all_four_router_tools_without_paste_tags() {
    let Some(sb) = Sandbox::new("paste-dispatch") else { return };
    let r = shift_fixture(&sb);
    std::fs::remove_file(sb.home.join("manual-submit")).unwrap();
    write(&sb.home.join("wrap-pastes"), "");
    sb.aca(&["start", &r]);
    assert!(sb.until(|| sb.event_names().iter().any(|e| e == "turn.done")));
    let words = format!("  用户原话\n「{}」\n`保留空白`  ", "好".repeat(1100));
    sb.aca(&["send", &r, "-m", &words]);
    assert!(sb.until(|| {
        sb.aca(&["events", "-n", "20"]).lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).any(|e| {
            e["event"] == "turn.started"
                && e["session"] == r
                && e["prompt"].as_str().is_some_and(|p| p.starts_with("\n\n<pasted_content id=\"") && p.contains(">\n  用户原话\n"))
        })
    }));
    let recipients = ["message", "ticket", "give"].map(|name| sb.aca(&["new", "fake", &sb.proj(), "-g", "project", "-t", name, "--hold"]));
    for text in four_tool_deliveries(&sb, &r, recipients) {
        assert!(text.starts_with("用户对「dispatcher」说(界面,"), "{text}");
        assert!(text.contains(&format!("):\n「{words}」\ndispatcher:")), "{text}");
        assert!(!text.contains("pasted_content"), "{text}");
    }
}

fn four_tool_deliveries(sb: &Sandbox, router: &str, [a, b, c]: [String; 3]) -> [String; 4] {
    use serde_json::json;
    let tool = |name, args| router_tool(sb, router, name, args);
    let mail = |id| sb.json(&["inbox", id, "--json"])[0]["text"].as_str().unwrap().to_owned();
    tool("message", json!({"to":a,"text":"去看"}));
    tool("ticket", json!({"title":"two","criteria":"ok","to":b}));
    tool("ticket", json!({"title":"three","criteria":"ok"}));
    tool("give", json!({"ticket":2,"to":c}));
    write(
        &sb.home.join("agent.sh"),
        "#!/bin/sh\nfor arg do last=$arg; done\nprintf %s \"$last\" > \"$ACA_HOME/worker-opening\"\nexec sleep 600\n",
    );
    tool("start", json!({"title":"four","say":"接手","tool":"fake"}));
    let opening = sb.home.join("worker-opening");
    assert!(sb.until(|| opening.exists()));
    [mail(&a), mail(&b), mail(&c), std::fs::read_to_string(opening).unwrap()]
}

#[test]
fn unwatched_results_reach_the_router_thread_without_ringing_until_work_needs_assignment() {
    let Some(sb) = Sandbox::new("router-results") else { return };
    let dir = sb.proj();
    sb.aca(&["group", "create", "p", &dir]);
    let r = sb.aca(&["new", "fake", &dir, "-g", "p", "-t", "dispatcher", "--hold"]);
    let w = sb.aca(&["new", "fake", &dir, "-g", "p", "-t", "worker", "--parent", "-"]);
    sb.aca(&["group", "router", "p", &r]);
    sb.aca(&["start", &r]);
    let result = |args: &[&str]| assert!(sb.as_session(&w, args, "").status.success());
    let give = |title| assert!(sb.as_session(&r, &["task", "add", "-t", title, "--to", &w], "").status.success());
    let inbox = || sb.json(&["inbox", &r, "--json"]);
    let view = || sb.aca(&["ui", "--snapshot", "150x34", "hj"]);
    let bell = sb.home.join(format!("state/inbox/{r}.bell"));
    assert!(sb.as_session(&r, &["inbox", "watch"], "{}").status.success());
    let baseline = std::fs::read(&bell).unwrap();
    give("first");
    sb.hook(&w, "", r#"{"hook_event_name":"Stop","last_assistant_message":"第一轮结果\n第二行省略"}"#);
    let screen = view();
    assert!(screen.contains("第一轮结果") && !screen.contains("第二行省略"), "{screen}");
    assert_eq!(inbox(), serde_json::json!([]));
    result(&["task", "done", "1", "验证通过"]);
    let screen = view();
    assert_eq!(screen.matches("验证通过").count(), 1, "{screen}");
    assert_eq!(inbox(), serde_json::json!([]));
    assert_eq!(std::fs::read(&bell).unwrap(), baseline);
    give("second");
    assert!(sb.as_session(&r, &["task", "add", "-t", "queued"], "").status.success());
    result(&["task", "done", "2", "可以继续"]);
    let messages = inbox();
    assert_eq!(messages.as_array().unwrap().len(), 1);
    assert_eq!(messages[0]["text"], "#2 完成:second\n可以继续");
    assert_eq!(messages[0]["wake"], true);
    let screen = view();
    assert_eq!(screen.matches("#2 完成").count(), 1, "{screen}");
}

#[test]
fn new_requires_a_ticket_for_each_watch_flag_before_creating_the_session() {
    let Some(sb) = Sandbox::new("new-watch") else { return };
    let proj = sb.proj();
    for flag in ["--watch", "--no-watch"] {
        for (lang, message) in [("zh", "要和 --task 一起用"), ("en", "goes with --task")] {
            let o = sb.cmd().env("ACA_LANG", lang).args(["new", "fake", &proj, "--hold", flag]).output().unwrap();
            assert!(!o.status.success() && o.stdout.is_empty(), "{o:?}");
            assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("{flag} {message}")), "{o:?}");
            assert_eq!(sb.json(&["ls", "--json"]), serde_json::json!([]));
        }
    }
    sb.aca(&["group", "create", "p", &proj]);
    for (n, flag, watch) in [("1", "--watch", true), ("2", "--no-watch", false)] {
        let before = if watch { "--no-watch" } else { "--watch" };
        sb.aca(&["task", "add", "-g", "p", "-t", "test", before]);
        let id = sb.aca(&["new", "fake", &proj, "--hold", "--task", n, flag]);
        let ticket = sb.json(&["task", "show", n, "--json"]);
        assert_eq!(ticket["watch"], watch);
        assert_eq!(ticket["session"], id);
    }
}

#[test]
fn watch_tools_ring_on_each_turn_until_the_pair_hits_the_brake() {
    use serde_json::json;
    let Some(sb) = Sandbox::new("router-watch") else { return };
    let dir = sb.proj();
    sb.aca(&["group", "create", "p", &dir]);
    let r = sb.aca(&["new", "fake", &dir, "-t", "dispatcher", "-g", "p", "--hold"]);
    let w = sb.aca(&["new", "fake", &dir, "-t", "worker", "-g", "p", "--parent", "-"]);
    sb.aca(&["group", "router", "p", &r]);
    sb.aca(&["start", &r]);
    let run = |name, args| router_tool(&sb, &r, name, args);
    let watched = |n| sb.json(&["task", "show", n, "--json"])["watch"] == true;
    assert_eq!(run("ticket", json!({"title":"first","criteria":"ok","to":w,"watch":true})), "#1");
    assert!(watched("1") && run("tickets", json!({})).contains("盯着"));
    assert!(sb.aca(&["task", "show", "1"]).contains("盯着"));
    assert!(sb.aca(&["ui", "--snapshot", "150x34", "hjl"]).contains("盯着"));
    // Restart to read the ticket from the registry in the opening handoff.
    write(
        &sb.home.join("agent.sh"),
        "#!/bin/sh\nif [ -n \"$ACA_ROUTER_SHIFT\" ]; then exec python3 \"$ACA_HOME/router.py\" \"$@\"; fi\nprintf '%s' \"$@\" > \"$ACA_HOME/open-$ACA_SESSION_ID\"\nexec sleep 600\n",
    );
    sb.aca(&["stop", &r]);
    sb.aca(&["start", &r]);
    assert!(sb.until(|| std::fs::read_to_string(sb.home.join(format!("open-{r}"))).is_ok_and(|s| s.contains("盯着"))));
    assert!(sb.as_session(&r, &["inbox", "watch"], "{}").status.success());
    let bell = || std::fs::read(sb.home.join(format!("state/inbox/{r}.bell"))).unwrap();
    let stop = |i| {
        sb.hook(&w, "", &json!({"hook_event_name":"UserPromptSubmit","prompt":"check"}).to_string());
        sb.hook(&w, "", &json!({"hook_event_name":"Stop","last_assistant_message":format!("result {i}")}).to_string());
    };
    for i in 1..=20 {
        if i == 2 {
            run("message", json!({"to":w,"text":"接着看"}));
        }
        let before = bell();
        stop(i);
        assert!(sb.until(|| bell() != before), "turn {i} did not ring");
        let take = sb.as_session(&r, &["inbox", "take"], "");
        assert_eq!(take.status.code(), Some(2));
        let msg = String::from_utf8(take.stderr).unwrap();
        assert!(msg.contains("#1") && msg.contains(&format!("result {i}")) && msg.contains(&format!("look {w}")), "{msg}");
    }
    let before = bell();
    stop(21);
    let messages = sb.json(&["inbox", &r, "-n", "30", "--json"]);
    assert_eq!(messages.as_array().unwrap().len(), 21);
    assert_eq!(messages[20]["wake"], false);
    assert!(messages[20]["text"].as_str().unwrap().contains("result 21"));
    assert_eq!(sb.as_session(&r, &["inbox", "take"], "").status.code(), Some(0));
    assert_eq!(bell(), before);
    assert_eq!(sb.event_names().iter().filter(|x| *x == "message.throttled").count(), 1);

    run("set", json!({"ticket":1,"watch":false}));
    assert!(!watched("1") && !run("tickets", json!({})).contains("盯着"));
    run("give", json!({"ticket":1,"to":"-","watch":true}));
    assert!(watched("1"));
    run("give", json!({"ticket":1,"to":w}));
    assert!(watched("1"));
    run("give", json!({"ticket":1,"to":w,"watch":false}));
    assert!(!watched("1"));
    assert!(!sb.as_session(&w, &["task", "set", "1", "--watch"], "").status.success());
    assert!(!sb.run(&["task", "set", "1", "--watch", "--no-watch"]).status.success());

    write(&sb.home.join("agent.sh"), "#!/bin/sh\n\"$ACA_BIN\" task ls --json > \"$ACA_HOME/at-start-$ACA_SESSION_ID\"\nexec sleep 600\n");
    for (n, args) in [
        ("2", json!({"title":"new","say":"do it","tool":"fake","watch":true})),
        ("1", json!({"ticket":1,"title":"assigned","say":"go","tool":"fake","watch":true})),
    ] {
        let reply = run("start", args);
        let id = reply.split_whitespace().nth(1).unwrap();
        assert!(watched(n));
        assert!(sb.until(|| {
            std::fs::read(sb.home.join(format!("at-start-{id}")))
                .ok()
                .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
                .is_some_and(|ts| ts.iter().any(|t| t["session"] == id && t["watch"] == true))
        }));
    }
    let old = sb.json(&["task", "show", "1", "--json"]);
    add_config(&sb, "[tools.broken]\nkind='claude'\ncmd=['missing-aca-watch-test-executable']\n");
    assert!(!sb.as_session(&r, &["new", "broken", &dir, "--task", "1", "--no-watch"], "").status.success());
    assert_eq!(sb.json(&["task", "show", "1", "--json"]), old);
}

#[test]
#[ignore = "set ACA_V5_BIN to an installed schema-5 aca binary"]
fn an_installed_v5_binary_reads_and_updates_a_v6_registry() {
    let Some(sb) = Sandbox::new("router-v5") else { return };
    let binary = std::env::var_os("ACA_V5_BIN").expect("ACA_V5_BIN");
    let old = |args: &[&str]| {
        let mut c = Command::new(&binary);
        let out = scrub(&mut c).env("ACA_HOME", &sb.home).env("ACA_TMUX_SOCKET", &sb.socket).args(args).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let version = || {
        let db = rusqlite::Connection::open(sb.home.join("data/aca.db")).unwrap();
        db.query_row("pragma user_version", [], |r| r.get::<_, i64>(0)).unwrap()
    };
    println!("{}", old(&["version"]));
    old(&["group", "create", "p", &sb.proj()]);
    assert_eq!(old(&["task", "add", "-g", "p", "-t", "old"]), "#1");
    assert_eq!(version(), 5);
    assert_eq!(sb.json(&["task", "show", "1", "--json"])["watch"], false);
    assert_eq!(version(), 6);
    sb.aca(&["task", "set", "1", "--watch"]);
    assert!(old(&["task", "ls"]).contains("old"));
    old(&["task", "set", "1", "--note", "old writer"]);
    assert_eq!(old(&["task", "add", "-g", "p", "-t", "second"]), "#2");
    let ticket = sb.json(&["task", "show", "1", "--json"]);
    assert_eq!(ticket["note"], "old writer");
    assert_eq!(ticket["watch"], true);
    assert_eq!(sb.json(&["task", "show", "2", "--json"])["watch"], false);
    assert_eq!(version(), 6);
    println!("v5 -> v6: existing ticket defaults off; old writer preserved watch and added an unwatched ticket");
}

fn shift_fixture(sb: &Sandbox) -> String {
    write(&sb.home.join("manual-submit"), "");
    add_config(sb, &format!("[tools.shift]\nkind='claude'\nprogram_status=false\ncmd=['python3',{:?}]\n", sb.home.join("router.py")));
    let id = sb.aca(&["new", "shift", &sb.proj(), "-g", "project", "-t", "dispatcher", "--hold"]);
    sb.aca(&["group", "router", "project", &id]);
    id
}

fn shift_prompt(sb: &Sandbox, conv: &str) -> String {
    let path = sb.home.join(format!("open-{conv}"));
    assert!(sb.until(|| std::fs::read_to_string(&path).is_ok_and(|s| s.starts_with("[aca] "))));
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn a_router_submits_the_literal_handoff_after_its_own_tools_list_or_timeout() {
    check_handoff_submit("handoff-list", false);
}

#[test]
fn a_wrapped_router_handoff_is_delivered_once_after_tools_list_or_timeout() {
    check_handoff_submit("wrapped-list", true);
}

fn check_handoff_submit(sandbox: &str, wrapped: bool) {
    use std::time::{Duration, Instant};
    let Some(sb) = Sandbox::new(sandbox) else { return };
    let id = shift_fixture(&sb);
    std::fs::remove_file(sb.home.join("manual-submit")).unwrap();
    if wrapped {
        write(&sb.home.join("wrap-pastes"), "");
    }
    write(&sb.home.join("delay-list"), "");
    let words = format!("「保留原话」\n`echo literal`\n{};  ", "行".repeat(7000));
    let log = sb.home.join("state/plugin-logs/aca.router.log");
    let mut previous = String::new();
    for timeout in [false, true] {
        if timeout {
            sb.aca(&["stop", &id]);
            write(&sb.home.join("skip-list"), "");
        }
        let start = Instant::now();
        let mut sender = sb.cmd().args(["msg", &id, &words]).stdout(std::process::Stdio::null()).spawn().unwrap();
        if timeout {
            assert!(sb.until(|| std::fs::read_to_string(sb.home.join("starts")).is_ok_and(|s| s.lines().count() == 2)));
            // A delayed tools/list from the previous shift cannot release this opening.
            let mut late = sb
                .cmd()
                .env("ACA_SESSION_ID", &id)
                .env("ACA_ROUTER_SHIFT", &previous)
                .args(["plugin", "tools"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            use std::io::Write;
            late.stdin.take().unwrap().write_all(b"{\"id\":1,\"method\":\"tools/list\"}\n").unwrap();
            assert!(String::from_utf8(late.wait_with_output().unwrap().stdout).unwrap().contains("aca-router__tickets"));
        }
        assert!(sender.wait().unwrap().success());
        let conv = sb.json(&["show", &id, "--json"])["conv"].as_str().unwrap().to_owned();
        let opening = shift_prompt(&sb, &conv);
        assert!(opening.ends_with(&words));
        let argv = std::fs::read_to_string(sb.home.join(format!("args-{id}"))).unwrap();
        assert!(!argv.contains("[aca]") && !argv.contains("保留原话"), "{argv}");
        let stamp = |name| std::fs::read_to_string(sb.home.join(format!("{name}-{conv}"))).map(|s| s.parse::<u64>().unwrap());
        if timeout {
            assert!(stamp("listed").is_err() && start.elapsed() >= Duration::from_secs(15));
        } else {
            assert!(stamp("listed").unwrap() <= stamp("received").unwrap() && start.elapsed() >= Duration::from_secs(2));
        }
        let kind = if timeout { "timeout" } else { "ready" };
        assert!(std::fs::read_to_string(&log).unwrap().contains(&format!("{conv} tools/list {kind} after ")));
        let mail = sb.json(&["inbox", &id, "--json"]);
        let message = mail.as_array().unwrap().last().unwrap()["id"].clone();
        let delivered = || {
            sb.aca(&["events", "-n", "100"])
                .lines()
                .map(|l| serde_json::from_str::<Value>(l).unwrap())
                .filter(|e| e["event"] == "message.delivered" && e["id"] == message)
                .map(|e| e["how"].clone())
                .collect::<Vec<_>>()
        };
        assert!(sb.until(|| delivered() == ["handoff"]));
        let prefix = if wrapped { "\n\n<pasted_content id=\"" } else { "[aca] " };
        assert!(sb.until(|| {
            sb.aca(&["events", "-n", "100"])
                .lines()
                .map(|l| serde_json::from_str::<Value>(l).unwrap())
                .filter(|e| e["event"] == "turn.started" && e["session"] == id && e["prompt"].as_str().is_some_and(|p| p.starts_with(prefix)))
                .count() == 1 + usize::from(timeout)
        }));
        assert_eq!(sb.as_session(&id, &["inbox", "take"], "").status.code(), Some(0));
        assert_eq!(delivered(), ["handoff"]);
        previous = conv;
    }
}

#[test]
fn three_wrapped_router_shifts_deliver_each_letter_once_without_echoing_the_handoff() {
    let Some(sb) = Sandbox::new("three-pastes") else { return };
    let router = shift_fixture(&sb);
    std::fs::remove_file(sb.home.join("manual-submit")).unwrap();
    write(&sb.home.join("wrap-pastes"), "");
    write(&sb.home.join("follow-bell"), "");
    let padding = "长".repeat(1050);
    let mail = [
        format!("第一封来信\n{padding}"),
        format!("第二封来信\n{padding}"),
        format!("第三封来信\n{padding}\n<pasted_content id=\"abcd\">原样</pasted_content id=\"abcd\">\n"),
    ];
    let mut shifts = std::collections::HashSet::new();
    for (i, letter) in mail.iter().enumerate() {
        if i != 0 {
            sb.aca(&["stop", &router]);
        }
        sb.aca(&["msg", &router, letter]);
        let conv = sb.json(&["show", &router, "--json"])["conv"].as_str().unwrap().to_owned();
        assert!(shifts.insert(conv.clone()));
        let opening = shift_prompt(&sb, &conv);
        assert!(opening.ends_with(letter));
        if i == 1 {
            assert!(opening.contains("第一封来信") && !opening.contains("pasted_content"), "{opening}");
        }
        assert!(sb.until(|| sb.event_names().iter().filter(|name| *name == "turn.done").count() == i + 1));
        let submitted = std::fs::read_to_string(sb.home.join(format!("submit-{conv}"))).unwrap();
        let head = submitted.strip_prefix("\n\n<pasted_content id=\"").unwrap();
        let (id, _) = head.split_once("\">\n").unwrap();
        assert!(id.len() == 4 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        let escaped = opening.replace("<pasted_content", r"<\pasted_content").replace("</pasted_content", r"<\/pasted_content");
        let separator = if opening.ends_with('\n') { "" } else { "\n" };
        assert_eq!(submitted, format!("\n\n<pasted_content id=\"{id}\">\n{escaped}{separator}</pasted_content id=\"{id}\">\n"));

        assert_eq!(sb.as_session(&router, &["inbox", "take"], "").status.code(), Some(0));
        let inbox = sb.json(&["inbox", &router, "--json"]);
        let message = &inbox.as_array().unwrap().iter().find(|m| &m["text"] == letter).unwrap()["id"];
        let delivered: Vec<_> = sb
            .aca(&["events", "-n", "100"])
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|e| e["event"] == "message.delivered" && e["id"] == *message)
            .map(|e| e["how"].clone())
            .collect();
        assert_eq!(delivered, ["handoff"]);
    }
}

#[test]
fn only_the_doorbell_submit_consumes_the_taken_channel() {
    use serde_json::json;
    let Some(sb) = Sandbox::new("bell-channel") else { return };
    let r = shift_fixture(&sb);
    std::fs::remove_file(sb.home.join("manual-submit")).unwrap();
    sb.aca(&["start", &r]);
    assert!(sb.until(|| sb.event_names().iter().any(|e| e == "turn.done")));
    leave_draft(&sb, &r);
    sb.aca(&["msg", "--via", "Discord", &r, "原话"]);
    assert_eq!(sb.as_session(&r, &["inbox", "take"], "").status.code(), Some(2));
    for prompt in [
        "用户输入",
        "<task-notification><summary>another task</summary></task-notification>",
        "<task-notification><summary>aca: 收件箱有新消息</summary></task-notification>",
    ] {
        sb.hook(&r, "", &json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}).to_string());
    }
    sb.hook(&r, "", r#"{"hook_event_name":"Stop","last_assistant_message":"channel reply"}"#);
    assert!(
        sb.aca(&["events", "-n", "30"])
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .any(|e| e["event"] == "turn.done" && e["session"] == r && e["last"] == "channel reply" && e["via"] == "Discord")
    );
}

#[test]
fn an_unmatched_handoff_leaves_its_mail_for_the_doorbell() {
    let Some(sb) = Sandbox::new("wrong-handoff") else { return };
    let id = shift_fixture(&sb);
    sb.aca(&["msg", &id, "original"]);
    let conv = sb.json(&["show", &id, "--json"])["conv"].as_str().unwrap().to_owned();
    let opening = shift_prompt(&sb, &conv);
    assert!(opening.ends_with("\noriginal"));
    assert!(sb.as_session(&id, &["inbox", "watch"], "{}").status.success());
    let bell = sb.home.join(format!("state/inbox/{id}.bell"));
    let before = std::fs::read(&bell).unwrap();
    sb.hook(&id, "", r#"{"hook_event_name":"UserPromptSubmit","prompt":"a different opening"}"#);
    assert_eq!(sb.json(&["inbox", &id, "--json"])[0]["stage"], "pending");
    leave_draft(&sb, &id);
    sb.hook(&id, "", r#"{"hook_event_name":"Stop","last_assistant_message":"好"}"#);
    assert!(sb.until(|| std::fs::read(&bell).unwrap() != before));
    let taken = sb.as_session(&id, &["inbox", "take"], "");
    assert_eq!(taken.status.code(), Some(2));
    assert!(String::from_utf8(taken.stderr).unwrap().contains("「original」"));
    let deliveries: Vec<_> = sb
        .aca(&["events", "-n", "50"])
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|e| e["event"] == "message.delivered")
        .map(|e| e["how"].clone())
        .collect();
    assert_eq!(deliveries, ["bell"]);
}

#[test]
fn a_router_with_gateway_tools_waits_for_its_input_box() {
    let Some(sb) = Sandbox::new("handoff-gateway") else { return };
    let id = shift_fixture(&sb);
    sb.aca(&["plugin", "install", "aca.gateway"]);
    write(&sb.home.join("hold-screen"), "");
    let mut start = sb.cmd().args(["start", &id]).stdout(std::process::Stdio::null()).spawn().unwrap();
    let log = sb.home.join("state/plugin-logs/aca.router.log");
    assert!(sb.until(|| std::fs::read_to_string(&log).is_ok_and(|s| s.contains("tools/list ready after "))));
    let mut late = sb.cmd().args(["msg", &id, "after empty opening"]).stdout(std::process::Stdio::null()).spawn().unwrap();
    assert!(sb.until(|| sb.json(&["inbox", &id, "--json"]).as_array().is_some_and(|m| m.len() == 1)));
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert!(start.try_wait().unwrap().is_none() && late.try_wait().unwrap().is_none());
    std::fs::remove_file(sb.home.join("hold-screen")).unwrap();
    assert!(start.wait().unwrap().success() && late.wait().unwrap().success());
    assert!(shift_prompt(&sb, sb.json(&["show", &id, "--json"])["conv"].as_str().unwrap()).ends_with("只回一个字。"));
    assert_eq!(sb.as_session(&id, &["inbox", "take"], "").status.code(), Some(0));
    sb.hook(&id, "", r#"{"hook_event_name":"UserPromptSubmit","prompt":"another prompt"}"#);
    assert_eq!(sb.as_session(&id, &["inbox", "take"], "").status.code(), Some(2));
    assert!(
        sb.aca(&["events", "-n", "50"])
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .any(|e| e["event"] == "message.delivered" && e["how"] == "bell")
    );
    sb.aca(&["plugin", "gateway", "stop"]);
}

#[test]
fn simultaneous_mail_and_mail_during_router_start_and_stop_reach_one_shift() {
    use serde_json::json;
    use std::process::Stdio;
    let Some(sb) = Sandbox::new("shift-race") else { return };
    let id = shift_fixture(&sb);
    let show = || sb.json(&["show", &id, "--json"]);
    let inbox = || sb.json(&["inbox", &id, "--json"]);
    let before = show();
    let starts = || std::fs::read_to_string(sb.home.join("starts")).unwrap_or_default().lines().count();
    let submit = |conv: &str, text: &str| {
        sb.hook(&id, "", &json!({"hook_event_name":"UserPromptSubmit","session_id":conv,"prompt":text}).to_string())
    };
    let spawn = |args: &[&str]| sb.cmd().args(args).stdout(Stdio::null()).spawn().unwrap();
    // Both real senders reach the stopped router before either can acquire its start lock.
    let lock = sb.home.join(format!("state/inbox/{id}.shift"));
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let gate = std::fs::File::options().create(true).append(true).open(lock).unwrap();
    gate.lock().unwrap();
    let mut a = spawn(&["msg", "--via", "Discord", &id, "  first\nline  "]);
    let mut b = spawn(&["msg", "--via", "Discord", &id, "第二句"]);
    assert!(sb.until(|| inbox().as_array().unwrap().len() == 2));
    drop(gate);
    assert!(a.wait().unwrap().success() && b.wait().unwrap().success());
    let current = show();
    let conv = current["conv"].as_str().unwrap();
    assert_ne!(current["conv"], before["conv"]);
    for key in ["id", "title", "group", "tool", "account"] {
        assert_eq!(current[key], before[key], "{key}");
    }
    assert_eq!(sb.aca(&["group", "router", "project"]), id);
    let opening = shift_prompt(&sb, conv);
    for text in ["  first\nline  ", "第二句"] {
        assert_eq!(opening.matches(text).count(), 1, "{opening}");
    }
    assert_eq!(opening.matches("用户 · Discord").count(), 2, "{opening}");
    assert_eq!(starts(), 1);
    // The new process received its paste but has not submitted it. The bell must not steal its opening batch.
    let late = "while starting";
    sb.aca(&["msg", &id, late]);
    assert_eq!(sb.as_session(&id, &["inbox", "take"], "").status.code(), Some(0));
    assert!(inbox().as_array().unwrap().iter().all(|m| m["stage"] == "pending"));
    submit(conv, &opening);
    let mail = inbox();
    let first = mail[0]["text"].as_str().unwrap();
    let worker = sb.aca(&["new", "fake", &sb.proj(), "--hold"]);
    assert!(sb.as_session(&id, &["msg", &worker, "dispatch"], "").status.success());
    let forwarded = sb.json(&["inbox", &worker, "--json"]);
    let words = forwarded[0]["text"].as_str().unwrap();
    assert!(words.contains("Discord,") && words.contains(&format!("「{first}」")), "{words}");
    assert_eq!((&mail[0]["stage"], &mail[1]["stage"], &mail[2]["stage"]), (&json!("got"), &json!("got"), &json!("pending")));
    let take = sb.as_session(&id, &["inbox", "take"], "");
    assert_eq!(take.status.code(), Some(2));
    assert!(String::from_utf8(take.stderr).unwrap().contains(late));
    assert_eq!(starts(), 1);

    // Synchronize on the agent actually processing SIGTERM, not a sleep before starting the second command.
    write(&sb.home.join("hold-stop"), "");
    let mut stopping = spawn(&["stop", &id]);
    assert!(sb.until(|| sb.home.join("stopping").exists()));
    let mut from_worker = sb.cmd().env("ACA_SESSION_ID", &worker).args(["msg", &id, "worker first"]).stdout(Stdio::null()).spawn().unwrap();
    assert!(sb.until(|| inbox().as_array().unwrap().len() == 4));
    let mut from_user = spawn(&["msg", &id, "user second"]);
    assert!(sb.until(|| inbox().as_array().unwrap().len() == 5));
    std::fs::remove_file(sb.home.join("hold-stop")).unwrap();
    assert!(stopping.wait().unwrap().success() && from_worker.wait().unwrap().success() && from_user.wait().unwrap().success());
    let next = show()["conv"].as_str().unwrap().to_string();
    assert_ne!(conv, next);
    let prompt = shift_prompt(&sb, &next);
    assert!(prompt.contains("worker first") && prompt.contains("user second") && prompt.contains(&worker), "{prompt}");
    assert_eq!(starts(), 2);
    // Exit before acknowledging the handoff: the next start must carry both originals again.
    sb.aca(&["stop", &id]);
    sb.aca(&["start", &id]);
    let final_conv = show()["conv"].as_str().unwrap().to_string();
    let prompt = shift_prompt(&sb, &final_conv);
    assert!(prompt.contains("worker first") && prompt.contains("user second"), "{prompt}");
    submit(&final_conv, &prompt);
    assert!(inbox().as_array().unwrap().iter().all(|m| m["stage"] == "got"));
    assert!(sb.as_session(&id, &["msg", &worker, "after worker"], "").status.success());
    assert_eq!(sb.json(&["inbox", &worker, "--json"])[1]["text"], "after worker");
    assert_eq!(sb.event_names().iter().filter(|s| s.as_str() == "session.started").count(), 3);
}

#[test]
fn router_delivery_rechecks_ten_minutes_and_an_expired_shift_without_a_ui() {
    use std::os::unix::fs::PermissionsExt;
    let Some(sb) = Sandbox::new("shift-idle") else { return };
    let id = shift_fixture(&sb);
    sb.aca(&["start", &id]);
    let show = || sb.json(&["show", &id, "--json"]);
    let first = show()["conv"].as_str().unwrap().to_owned();
    let opening = shift_prompt(&sb, &first);
    sb.hook(&id, "", &serde_json::json!({"hook_event_name":"UserPromptSubmit","session_id":first,"prompt":opening}).to_string());
    sb.hook(&id, "", r#"{"hook_event_name":"Stop","last_assistant_message":"ready"}"#);
    assert!(sb.as_session(&id, &["inbox", "watch"], "{}").status.success());
    // Keep the real pane, pid, screen and tmux operations; backdate just its three read-only activity clocks.
    let shim = sb.home.join("bin/tmux");
    write(
        &shim,
        r#"#!/bin/sh
if [ "$3" = list-panes ]; then
    format=$(printf '%s' "$6" | sed -e "s/#{session_activity}/$SHIFT_AT/g" -e "s/#{session_last_attached}/$SHIFT_AT/g" -e "s/#{window_activity}/$SHIFT_AT/g")
    exec "$REAL_TMUX" "$1" "$2" "$3" "$4" "$5" "$format"
fi
exec "$REAL_TMUX" "$@"
"#,
    );
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var("PATH").unwrap();
    let tmux = std::env::split_paths(&path).map(|d| d.join("tmux")).find(|f| f.is_file()).unwrap();
    let send_at_age = |age: u64, text: &str| {
        let at = aca_core::paths::now() - age;
        let db = rusqlite::Connection::open(sb.home.join("data/aca.db")).unwrap();
        db.execute("update sessions set created_at=?1 where id=?2", rusqlite::params![at as i64, id]).unwrap();
        let file = sb.home.join(format!("state/run/{id}.json"));
        let mut run: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        for key in ["started", "ts", "turn", "done"] {
            run[key] = at.into();
        }
        write(&file, &run.to_string());
        let result = sb
            .cmd()
            .env("PATH", format!("{}:{path}", shim.parent().unwrap().display()))
            .env("REAL_TMUX", &tmux)
            .env("SHIFT_AT", at.to_string())
            .args(["msg", &id, text])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        String::from_utf8(result.stdout).unwrap()
    };
    assert!(send_at_age(600, "warm").contains("已输入并发送"));
    assert_eq!(show()["conv"], first);
    assert_eq!(sb.as_session(&id, &["inbox", "take"], "").status.code(), Some(0));
    assert!(send_at_age(3601, "cold").contains("新一班"));
    let next = show()["conv"].as_str().unwrap().to_string();
    assert_ne!(next, first);
    assert!(shift_prompt(&sb, &next).ends_with("\ncold"));
}

#[test]
fn codex_router_keeps_stopped_mail_and_its_resume_arguments() {
    let Some(sb) = Sandbox::new("shift-codex") else { return };
    write(
        &sb.home.join("cx.sh"),
        r#"#!/bin/sh
for a do [ "$a" != queue ] || exit 1; done
printf '%s\0' "$@" > "$ACA_HOME/codex-args"
exec sleep 600
"#,
    );
    add_config(&sb, &format!("[tools.cx]\nkind='codex'\ncmd=['/bin/sh',{:?}]\n[router]\nidle='1s'\n", sb.home.join("cx.sh")));
    let id = sb.aca(&["new", "cx", &sb.proj(), "-g", "p", "--hold"]);
    sb.aca(&["group", "router", "p", &id]);
    sb.aca(&["msg", &id, "held"]);
    assert_eq!(sb.json(&["inbox", &id, "--json"])[0]["stage"], "pending");
    assert_eq!(sb.json(&["show", &id, "--json"])["state"], "stopped");
    sb.aca(&["start", &id]);
    sb.hook_as("codex", &id, "", r#"{"hook_event_name":"SessionStart","session_id":"saved-thread"}"#);
    let args = sb.home.join("codex-args");
    let read_resume = || {
        sb.aca(&["stop", &id]);
        std::fs::remove_file(&args).unwrap();
        sb.aca(&["start", &id]);
        assert!(sb.until(|| std::fs::read(&args).is_ok_and(|s| s.windows(13).any(|w| w == b"saved-thread\0"))));
        std::fs::read(&args).unwrap()
    };
    let before = read_resume();
    assert!(before.windows(7).any(|w| w == b"resume\0"));
    let after = read_resume();
    assert_eq!(after, before);
    assert!(!String::from_utf8(after).unwrap().contains("交接"));
    std::thread::sleep(std::time::Duration::from_secs(2));
    sb.aca(&["stop", "--idle", "1s"]);
    assert_ne!(sb.json(&["show", &id, "--json"])["state"], "stopped");
    assert_ne!(sb.json(&["show", &id, "--json"])["state"], "exited");
}

#[test]
fn tickets_watch_notices_and_a_due_timer_start_stopped_router_shifts() {
    use serde_json::json;
    let Some(sb) = Sandbox::new("shift-notices") else { return };
    let id = shift_fixture(&sb);
    let worker = sb.aca(&["new", "fake", &sb.proj(), "-g", "project", "--parent", "-"]);
    let accept = |message: &str| {
        assert!(sb.until(|| !matches!(sb.json(&["show", &id, "--json"])["state"].as_str(), Some("stopped" | "exited"))));
        let conv = sb.json(&["show", &id, "--json"])["conv"].as_str().unwrap().to_owned();
        let prompt = shift_prompt(&sb, &conv);
        assert!(prompt.split("叫醒你的\n").nth(1).is_some_and(|body| body.contains(message)), "{prompt}");
        sb.hook(&id, "", &json!({"hook_event_name":"UserPromptSubmit","session_id":conv,"prompt":prompt}).to_string());
        assert!(sb.as_session(&id, &["msg", &worker, "dispatch"], "").status.success());
        let mail = sb.json(&["inbox", &worker, "--json"]);
        assert_eq!(mail.as_array().unwrap().last().unwrap()["text"], "dispatch");
        sb.aca(&["stop", &id]);
    };
    sb.aca(&["task", "add", "-g", "project", "-t", "queued"]);
    accept("#1 新任务");
    sb.aca(&["task", "add", "-g", "project", "-t", "plain", "--to", &worker]);
    assert!(sb.as_session(&worker, &["task", "done", "2", "finished"], "").status.success());
    accept("#2 完成");
    sb.aca(&["task", "add", "-g", "project", "-t", "watched", "--watch", "--to", &worker]);
    for (event, fields, text) in [
        ("Stop", json!({"last_assistant_message":"still working"}), "still working"),
        ("PreToolUse", json!({"tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"green?"}]}}), "green?"),
        ("StopFailure", json!({"error":"failed now"}), "failed now"),
        ("SessionEnd", json!({"reason":"prompt_input_exit"}), "#3"),
    ] {
        sb.hook(&worker, "", r#"{"hook_event_name":"UserPromptSubmit","prompt":"work"}"#);
        let mut payload = fields;
        payload["hook_event_name"] = event.into();
        sb.hook(&worker, "", &payload.to_string());
        accept(text);
    }
    assert!(sb.as_session(&worker, &["task", "done", "3", "all good"], "").status.success());
    accept("#3 完成");
    assert!(sb.as_session(&id, &["msg", "--in", "1s", &id, "check again"], "").status.success());
    accept("check again");
}

#[test]
fn mail_arriving_while_tmux_is_starting_the_router_waits_for_its_turn() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    let Some(sb) = Sandbox::new("shift-launch") else { return };
    let id = shift_fixture(&sb);
    let mut log = std::fs::File::options().append(true).open(sb.home.join("state/events.jsonl")).unwrap();
    for n in 0..17 {
        writeln!(log, "{}", serde_json::json!({"ts":100000+n,"event":"turn.done","session":id,"last":format!("reply-{n:02}")})).unwrap();
    }
    drop(log);
    let shim = sb.home.join("bin/tmux");
    write(
        &shim,
        r#"#!/bin/sh
if [ "$3" = new-session ]; then
    touch "$ACA_HOME/launching"
    while [ -f "$ACA_HOME/launching" ]; do sleep 0.05; done
fi
exec "$REAL_TMUX" "$@"
"#,
    );
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var("PATH").unwrap();
    let tmux = std::env::split_paths(&path).map(|d| d.join("tmux")).find(|f| f.is_file()).unwrap();
    let mut launch = sb
        .cmd()
        .env("PATH", format!("{}:{path}", shim.parent().unwrap().display()))
        .env("REAL_TMUX", tmux)
        .args(["msg", &id, "first"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    assert!(sb.until(|| sb.home.join("launching").exists()));
    let mut late = sb.cmd().args(["msg", &id, "late"]).stdout(Stdio::null()).spawn().unwrap();
    let mut manual = sb.cmd().args(["start", &id]).stdout(Stdio::null()).spawn().unwrap();
    assert!(sb.until(|| sb.json(&["inbox", &id, "--json"]).as_array().unwrap().len() == 2));
    assert!(launch.try_wait().unwrap().is_none() && late.try_wait().unwrap().is_none());
    std::fs::remove_file(sb.home.join("launching")).unwrap();
    assert!(launch.wait().unwrap().success() && late.wait().unwrap().success() && manual.wait().unwrap().success());
    let current = sb.json(&["show", &id, "--json"]);
    let conv = current["conv"].as_str().unwrap();
    let opening = shift_prompt(&sb, conv);
    assert!(opening.ends_with("\nfirst") && !opening.contains("late"), "{opening}");
    assert_eq!(opening.matches("reply-").count(), 15);
    assert!(!opening.contains("reply-01") && opening.contains("reply-02") && opening.contains("reply-16"));
    sb.hook(&id, "", &serde_json::json!({"hook_event_name":"UserPromptSubmit","session_id":conv,"prompt":opening}).to_string());
    let take = sb.as_session(&id, &["inbox", "take"], "");
    assert_eq!(take.status.code(), Some(2));
    assert!(String::from_utf8(take.stderr).unwrap().contains("「late」"));
    assert!(sb.json(&["inbox", &id, "--json"]).as_array().unwrap().iter().all(|m| m["stage"] == "got"));
    assert_eq!(sb.event_names().iter().filter(|e| *e == "session.started").count(), 1);
}

#[test]
fn a_router_woken_on_a_cold_tmux_server_only_passes_its_own_identity_to_the_pane() {
    let Some(sb) = Sandbox::new("cold-server-env") else { return };
    write(
        &sb.home.join("agent.sh"),
        r#"#!/bin/sh
dump="$ACA_HOME/env-$ACA_SESSION_ID"
env | sed -n '/^ACA_/p' > "$dump.tmp" && mv "$dump.tmp" "$dump"
if [ -n "$ACA_ROUTER_SHIFT" ]; then exec python3 "$ACA_HOME/router.py" "$@"; fi
exec sleep 600
"#,
    );
    let sender = sb.aca(&["new", "fake", &sb.proj(), "--hold"]);
    let router = sb.aca(&["new", "fake", &sb.proj(), "-g", "project", "--hold"]);
    sb.aca(&["group", "router", "project", &router]);
    assert!(sb.tmux(&["show-environment", "-g", "PATH"]).is_empty());
    let private = [
        ("ACA_SETTING_TOKEN", "x"),
        ("ACA_SETTING_URL", "fake-url"),
        ("ACA_PLUGIN_EVENT", "y"),
        ("ACA_ARG_TO", "z"),
        ("ACA_INPUT_FILE", "input"),
        ("ACA_ROUTER_GROUP", "sender-group"),
        ("ACA_SESSION_PARENT", "sender-parent"),
        ("ACA_INVOKED_BY", "agent"),
        ("ACA_ENV", "1"),
    ];
    let out = sb
        .cmd()
        .envs(private)
        .env("ACA_SESSION_ID", &sender)
        .env("ACA_ROUTER_SHIFT", "sender-shift")
        .env("ACA_TOOL", "sender-tool")
        .env("ACA_BIN", "sender-bin")
        .args(["msg", &router, "wake up"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(sb.json(&["inbox", &router, "--json"])[0]["from"], sender);
    let shift = sb.json(&["show", &router, "--json"])["conv"].as_str().unwrap().to_owned();
    assert!(!shift.is_empty());
    let pid = sb.tmux(&["display-message", "-p", "#{pid}"]);
    let ordinary = sb.aca(&["new", "fake", &sb.proj()]);
    assert!(!pid.is_empty() && sb.tmux(&["display-message", "-p", "#{pid}"]) == pid);

    let pane_env = |id: &str| {
        let dump = sb.home.join(format!("env-{id}"));
        assert!(sb.until(|| dump.exists()));
        std::fs::read_to_string(dump).unwrap()
    };
    for (id, dump) in
        [("", sb.tmux(&["show-environment", "-g"])), (router.as_str(), pane_env(&router)), (ordinary.as_str(), pane_env(&ordinary))]
    {
        let vars: std::collections::HashMap<_, _> = dump.lines().filter_map(|line| line.split_once('=')).collect();
        for (key, _) in private {
            assert!(!vars.contains_key(key), "{key} in {id:?}");
        }
        assert_eq!(vars.get("ACA_LANG"), Some(&"zh"));
        assert_eq!(vars.get("ACA_HOME").copied(), sb.home.to_str());
        assert_eq!(vars.get("ACA_TMUX_SOCKET").copied(), Some(sb.socket.as_str()));
        assert_eq!(vars.get("ACA_ROUTER_SHIFT").copied(), (id == router).then_some(shift.as_str()));
        assert_eq!(vars.get("ACA_SESSION_ID").copied(), (!id.is_empty()).then_some(id));
        assert_eq!(vars.get("ACA_TOOL").copied(), (!id.is_empty()).then_some("fake"));
        assert_eq!(vars.get("ACA_BIN").copied(), (!id.is_empty()).then_some(env!("CARGO_BIN_EXE_aca")));
    }
}

#[test]
fn discord_posts_complete_channel_replies_in_order_and_keeps_ordinary_previews() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let Some(sb) = Sandbox::new("discord-post") else { return };
    assert!(Command::new("jq").arg("--version").output().unwrap().status.success());
    let curl = sb.home.join("bin/curl");
    write(
        &curl,
        r#"#!/bin/sh
for body do :; done
printf '%s\n' "$body" | jq -c . >> "$CAPTURE"
"#,
    );
    let aca = sb.home.join("bin/aca");
    write(&aca, "#!/bin/sh\nprintf '%s\\n' \"$ROW\"\n");
    for f in [&curl, &aca] {
        std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let capture = sb.home.join("posts");
    let payloads = |event: &str, data: Value, attached: bool, mention: &str| {
        write(&capture, "");
        let out = Command::new("sh")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/discord/post.sh"))
            .env("PATH", format!("{}:{}", curl.parent().unwrap().display(), std::env::var("PATH").unwrap()))
            .env("ACA_BIN", &aca)
            .env("ACA_SETTING_URL", "fake-webhook")
            .env("ACA_SETTING_MENTION", mention)
            .env("ACA_SESSION_ID", "desk")
            .env("ACA_PLUGIN_EVENT", event)
            .env("ACA_PLUGIN_EVENT_JSON", data.to_string())
            .env("ROW", json!({"attached":u8::from(attached),"title":"desk"}).to_string())
            .env("CAPTURE", &capture)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        std::fs::read_to_string(&capture).unwrap().lines().map(|line| serde_json::from_str::<Value>(line).unwrap()).collect::<Vec<_>>()
    };
    let post = |event, data, attached| {
        payloads(event, data, attached, "").into_iter().map(|v| v["content"].as_str().unwrap().to_owned()).collect::<Vec<_>>()
    };
    let text = "字".repeat(351) + "\n  quoted \"reply\"  \n";
    assert_eq!(post("turn.done", json!({"via":"Discord","last":text}), true), [text.as_str()]);
    assert_eq!(post("turn.done", json!({"last":text}), false), [format!("**✓ desk**\n{}", "字".repeat(300))]);
    assert!(post("turn.done", json!({"via":"Matrix","last":text}), true).is_empty());
    let first = "路".repeat(1999) + "\n";
    let second = " ".to_owned() + &"🌱".repeat(1998) + "\n";
    assert_eq!(post("turn.done", json!({"via":"Discord","last":format!("{first}{second}")}), true), [first, second]);
    assert_eq!(post("agent.waiting", json!({"via":"Discord","ask":"Which?"}), true), ["Which?"]);
    assert_eq!(post("turn.failed", json!({"via":"Discord","error":"connection lost"}), true), ["connection lost"]);
    let id = "123456789012345678";
    let mention = format!("<@{id}> ");
    let first = "问".repeat(2000 - mention.len());
    let rest = "\n<@999> 🌱\n";
    let ask = json!({"via":"Discord","ask":format!("{first}{rest}")});
    assert_eq!(
        payloads("agent.waiting", ask, true, id),
        [
            json!({"username":"aca","content":format!("{mention}{first}"),"allowed_mentions":{"parse":[],"users":[id]}}),
            json!({"username":"aca","content":rest,"allowed_mentions":{"parse":[]}}),
        ]
    );
    for (event, field, mention) in [("turn.done", "last", id), ("turn.failed", "error", id), ("agent.waiting", "ask", "")] {
        assert_eq!(
            payloads(event, json!({"via":"Discord",field:"<@999>"}), true, mention),
            [json!({"username":"aca","content":"<@999>","allowed_mentions":{"parse":[]}})]
        );
    }
}

#[test]
fn discord_wakes_the_desk_and_only_its_channel_turns_return_via_discord() {
    use serde_json::json;
    let Some(sb) = Sandbox::new("discord-shift") else { return };
    let id = shift_fixture(&sb);
    sb.aca(&["group", "dir", "project", &sb.proj()]);
    add_config(&sb, &format!("[scratch]\ndir={:?}\n", sb.proj()));
    sb.aca(&["msg", "desk", "--via", "Discord", "dispatch now"]);
    let conv = sb.json(&["show", &id, "--json"])["conv"].as_str().unwrap().to_owned();
    let opening = shift_prompt(&sb, &conv);
    assert!(opening.contains("总台交接") && opening.contains("用户 · Discord"));
    let hook = |v: Value| sb.hook(&id, "", &v.to_string());
    let events = || sb.aca(&["events", "-n", "100"]).lines().map(|s| serde_json::from_str::<Value>(s).unwrap()).collect::<Vec<_>>();
    let last = |name: &str| events().into_iter().rev().find(|v| v["event"] == name && v["session"] == id).unwrap();
    hook(json!({"hook_event_name":"UserPromptSubmit","session_id":conv,"prompt":opening}));
    hook(json!({"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which?"}]}}));
    assert_eq!(last("agent.waiting")["via"], "Discord");
    hook(json!({"hook_event_name":"Stop","last_assistant_message":"confirmed"}));
    assert_eq!(last("turn.done")["via"], "Discord");
    hook(json!({"hook_event_name":"UserPromptSubmit","prompt":"local input"}));
    hook(json!({"hook_event_name":"Stop","last_assistant_message":"local reply"}));
    assert!(last("turn.done").get("via").is_none());
    assert!(sb.as_session(&id, &["inbox", "watch"], "{}").status.success());
    sb.aca(&["msg", "desk", "--via", "Discord", "warm shift"]);
    assert_eq!(sb.as_session(&id, &["inbox", "take"], "").status.code(), Some(0));
    hook(json!({"hook_event_name":"UserPromptSubmit","prompt":"warm shift"}));
    hook(json!({"hook_event_name":"StopFailure","error":"timeout"}));
    assert_eq!(last("turn.failed")["via"], "Discord");
    hook(json!({"hook_event_name":"UserPromptSubmit","prompt":"ordinary turn"}));
    sb.aca(&["msg", "desk", "--via", "Discord", "mid-turn addition"]);
    assert_eq!(sb.as_session(&id, &["inbox", "take"], "").status.code(), Some(2));
    hook(json!({"hook_event_name":"Stop","last_assistant_message":"ordinary reply"}));
    assert!(last("turn.done").get("via").is_none());
}

#[test]
fn channel_mail_to_ordinary_agents_preserves_the_input_and_tracks_its_turn() {
    use serde_json::json;
    let Some(sb) = Sandbox::new("ordinary-via") else { return };
    sb.screen_tools();
    let words = "say | $ and quotes \" as written";
    for (tool, kind) in [("scr", "claude"), ("fcx", "codex")] {
        let id = sb.aca(&["new", tool, &sb.proj()]);
        let pane = sb.pane(&id);
        sb.hook_as(kind, &id, &pane, r#"{"hook_event_name":"SessionStart","session_id":"channel-thread"}"#);
        if kind == "claude" {
            sb.set_screen(&id, &pane, "ready\n❯ ");
        }
        let sent = sb.aca(&["msg", "--via", "Matrix", &id, words]);
        if kind == "codex" {
            assert!(sb.queued().contains(&format!("--message\n{words}\n")));
        } else {
            assert!(sent.contains("已输入并发送"), "{sent}");
            assert!(sb.wait_screen(&pane, words).contains(words));
        }
        let submit = |text| sb.hook_as(kind, &id, &pane, &json!({"hook_event_name":"UserPromptSubmit","prompt":text}).to_string());
        let done = || {
            sb.hook_as(kind, &id, &pane, r#"{"hook_event_name":"Stop","last_assistant_message":"received"}"#);
            sb.aca(&["events", "-n", "50"])
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .rfind(|e| e["session"] == id && e["event"] == "turn.done")
                .unwrap()
        };
        submit(words);
        assert_eq!(done()["via"], "Matrix");
        submit("local");
        assert!(done().get("via").is_none());
        assert_eq!(sb.json(&["inbox", &id, "--json"])[0]["text"], words);
    }
}

/// A cold router's new shift dies before its input box comes up, so delivering the message fails — but the message is in
/// its inbox already. `aca msg` says it's held (exit 0) instead of failing, which would have a relay like the Discord plugin
/// send it again the next minute and the inbox hold it twice; a retry 20 seconds later starts another shift and hands it over.
/// Errors from before it's put in still fail the command
#[test]
fn a_message_already_in_the_inbox_is_held_not_failed_when_delivering_it_fails() {
    use serde_json::json;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let Some(sb) = Sandbox::new("held-on-error") else { return };
    let id = shift_fixture(&sb);
    let text = "冷启动时的一句";
    // The new shift stays on "loading" and its pane is closed once it has listed its tools: the handoff never finds the input box
    write(&sb.home.join("hold-screen"), "");
    let msg = sb.cmd().args(["msg", "--via", "Discord", &id, text]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let log = sb.home.join("state/plugin-logs/aca.router.log");
    assert!(sb.until(|| std::fs::read_to_string(&log).is_ok_and(|s| s.contains("tools/list ready after "))));
    let pane = sb.tmux(&["list-panes", "-a", "-F", "#{pane_id}"]);
    sb.tmux(&["kill-pane", "-t", &pane]);
    let o = msg.wait_with_output().unwrap();
    let (out, err) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "aca msg 退出码 {:?}:{out}{err}", o.status.code());
    assert!(out.contains("暂存:无法送达(会话未运行)"), "{out}");
    let inbox = || sb.json(&["inbox", &id, "--json"]);
    let mail = inbox();
    assert_eq!(mail.as_array().unwrap().len(), 1, "{mail}");
    assert_eq!((mail[0]["text"].as_str(), mail[0]["stage"].as_str()), (Some(text), Some("pending")), "{mail}");

    // The retry: this shift comes up, gets the message in its opening once, and it's delivered
    std::fs::remove_file(sb.home.join("hold-screen")).unwrap();
    let opening = sb.home.join(format!("open-{id}"));
    let t = Instant::now();
    while !std::fs::read_to_string(&opening).is_ok_and(|s| s.contains(text)) {
        assert!(t.elapsed() < Duration::from_secs(45), "重试没有送达:{}", inbox());
        std::thread::sleep(Duration::from_millis(200));
    }
    let opening = std::fs::read_to_string(&opening).unwrap();
    assert_eq!(opening.matches(text).count(), 1, "{opening}");
    let conv = sb.json(&["show", &id, "--json"])["conv"].as_str().unwrap().to_string();
    sb.hook(&id, "", &json!({"hook_event_name":"UserPromptSubmit","session_id":conv,"prompt":opening}).to_string());
    let mail = inbox();
    assert_eq!((mail.as_array().unwrap().len(), mail[0]["stage"].as_str()), (1, Some("got")), "{mail}");
    assert_eq!(std::fs::read_to_string(sb.home.join("starts")).unwrap().lines().count(), 2);

    // Not put in at all: still a failure
    let o = sb.run(&["msg", "nope", "x"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("没有会话 nope"), "{err}");
    assert_eq!(sb.json(&["inbox", &id, "--json"]).as_array().unwrap().len(), 1);
}

/// `aca events -f` while the log rotates: the lines that went into the old file after it last looked, and those already in
/// the new one when it notices, are each printed once and in order. It's stopped (SIGSTOP) across the rotation so it can't
/// look in between, as if its 100 ms nap had fallen right there
#[test]
fn following_the_events_across_a_rotation_prints_every_line_once_and_in_order() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    let Some(sb) = Sandbox::new("events-rotate") else { return };
    let sh = sb.aca(&["new", "shell", &sb.proj()]);
    let rotated = sb.home.join("state/events.jsonl.1");
    // A little under the 4 MB limit: the next few events go into this file and take it over, the one after rotates it
    let mut f = std::fs::OpenOptions::new().append(true).open(sb.home.join("state/events.jsonl")).unwrap();
    let pad = "x".repeat(200);
    let mut n = 0;
    while f.metadata().unwrap().len() < 4 * 1024 * 1024 - 3000 {
        writeln!(f, "{{\"ts\":0,\"event\":\"fill\",\"session\":\"\",\"n\":{n},\"pad\":\"{pad}\"}}").unwrap();
        n += 1;
    }
    drop(f);

    struct Follower(std::process::Child);
    impl Drop for Follower {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut follower = Follower(sb.cmd().args(["events", "-f", "-n", "1"]).stdout(Stdio::piped()).spawn().unwrap());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let (out, lines) = (follower.0.stdout.take().unwrap(), seen.clone());
    std::thread::spawn(move || BufReader::new(out).lines().map_while(Result::ok).for_each(|l| lines.lock().unwrap().push(l)));
    let marks = || -> Vec<String> {
        let seen = seen.lock().unwrap();
        let events = seen.iter().filter_map(|l| serde_json::from_str::<Value>(l).ok());
        events.filter(|e| e["event"] == "message.sent").map(|e| e["text"].as_str().unwrap_or("").to_string()).collect()
    };
    // It's up: the last line already there
    let last = format!("\"n\":{},", n - 1);
    assert!(sb.until(|| seen.lock().unwrap().iter().any(|l| l.contains(&last))));
    let say = |mark: &str| {
        sb.aca(&["msg", &sh, mark]);
    };
    let mut want = vec!["mark-00".to_string()];
    say("mark-00");
    assert!(sb.until(|| marks() == want), "{:?}", marks());

    let pid = follower.0.id().to_string();
    let signal = |sig: &str| assert!(Command::new("kill").args([sig, &pid]).status().unwrap().success());
    signal("-STOP");
    // While it can't look: events into the old file, the rotation, events into the new one
    let (mut before, mut after) = (0, 0);
    for i in 1..60 {
        let mark = format!("mark-{i:02}");
        say(&mark);
        want.push(mark);
        if !rotated.exists() {
            before += 1;
        } else {
            after += 1;
            if after == 3 {
                break;
            }
        }
    }
    assert!(before > 0 && after == 3, "轮转前 {before} 条,轮转后 {after} 条");
    signal("-CONT");
    // And one more once it's looking again
    say("mark-last");
    want.push("mark-last".into());
    sb.until(|| marks().last().is_some_and(|m| m == "mark-last"));
    assert_eq!(marks(), want);
}

/// The new file can grow past where the old one was read to before `aca events -f` looks again: it isn't any smaller, but
/// it's another file, so it's read from its start. Then a second rotation while it's looking
#[test]
fn following_the_events_reads_a_new_file_from_its_start_even_once_it_outgrew_the_old_one() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    let Some(sb) = Sandbox::new("events-regrow") else { return };
    let sh = sb.aca(&["new", "shell", &sb.proj()]);
    let live = sb.home.join("state/events.jsonl");
    let pad = "x".repeat(200);
    let fill = |tag: &str, to: u64| {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&live).unwrap();
        let mut n = 0;
        while f.metadata().unwrap().len() <= to {
            writeln!(f, "{{\"ts\":0,\"event\":\"{tag}\",\"session\":\"\",\"n\":{n},\"pad\":\"{pad}\"}}").unwrap();
            n += 1;
        }
        n
    };
    // Over the limit already: the next event rotates it
    let old = fill("fill", 4 * 1024 * 1024);
    struct Follower(std::process::Child);
    impl Drop for Follower {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut follower = Follower(sb.cmd().args(["events", "-f", "-n", "1"]).stdout(Stdio::piped()).spawn().unwrap());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let (out, lines) = (follower.0.stdout.take().unwrap(), seen.clone());
    std::thread::spawn(move || BufReader::new(out).lines().map_while(Result::ok).for_each(|l| lines.lock().unwrap().push(l)));
    let events = || -> Vec<Value> { seen.lock().unwrap().iter().filter_map(|l| serde_json::from_str(l).ok()).collect() };
    let last = format!("\"n\":{},", old - 1);
    assert!(sb.until(|| seen.lock().unwrap().iter().any(|l| l.contains(&last))));

    let pid = follower.0.id().to_string();
    let signal = |sig: &str| assert!(Command::new("kill").args([sig, &pid]).status().unwrap().success());
    signal("-STOP");
    sb.aca(&["msg", &sh, "mark-a"]);
    assert!(sb.home.join("state/events.jsonl.1").exists());
    // The new file, past the old one's size before it looks again
    let grown = fill("grown", 4 * 1024 * 1024 + 10_000);
    signal("-CONT");
    let marks = || -> Vec<String> {
        events().iter().filter(|e| e["event"] == "message.sent").map(|e| e["text"].as_str().unwrap_or("").to_string()).collect()
    };
    let grown_seen = || events().iter().filter(|e| e["event"] == "grown").map(|e| e["n"].as_u64().unwrap()).collect::<Vec<_>>();
    sb.until(|| grown_seen().len() as u64 >= grown);
    let broken = seen.lock().unwrap().iter().filter(|l| serde_json::from_str::<Value>(l).is_err()).count();
    assert_eq!((marks(), grown_seen().len() as u64, broken), (vec!["mark-a".to_string()], grown, 0));
    assert_eq!(grown_seen(), (0..grown).collect::<Vec<_>>());
    // That one is over the limit too: the next event rotates it while it's looking
    sb.aca(&["msg", &sh, "mark-b"]);
    sb.until(|| marks().len() >= 2);
    assert_eq!(marks(), ["mark-a", "mark-b"]);
    assert_eq!(grown_seen().len() as u64, grown);
}
