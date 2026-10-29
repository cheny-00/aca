//! Runs it all end to end from the command line: starts real tmux sessions (with a fake agent inside), sends hooks, and checks
//! states, titles and events.
//!
//! Each test has its own `ACA_HOME` and its own tmux server (`-L`): they don't affect each other and can't touch real sessions.
//! Skipped when the machine has no tmux.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

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
        std::fs::write(home.join("agent.sh"), "#!/bin/sh\necho fake agent up\nexec sleep 600\n").unwrap();
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
        let o = scrub(&mut c).args(["-L", &self.socket]).args(args).output().unwrap();
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
    // viewed, folded ones) changes with it
    let ui = sb.home.join("state/ui");
    write(&ui.join("spaces"), "group\tdefault\taca-2\th\ngroup\tdefault\taca\t2\n");
    write(&ui.join("view"), "group=aca-2\nfolded=aca-2\tother\n");
    assert_eq!(sb.aca(&["group", "rename", "aca-2", "lab/aca"]), "aca-2 → lab/aca,1 个会话一并移动");
    assert_eq!(sb.json(&["show", &id, "--json"])["group"], "lab/aca");
    let spaces = std::fs::read_to_string(ui.join("spaces")).unwrap();
    assert!(spaces.contains("group\tdefault\tlab/aca\th\n") && spaces.contains("group\tdefault\taca\t2\n"), "{spaces}");
    let view = std::fs::read_to_string(ui.join("view")).unwrap();
    assert!(view.contains("group=lab/aca\n") && view.contains("folded=lab/aca\tother\n"), "{view}");
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

/// You decide: capabilities written as agent? go to agents only once you name them; you can change or remove keys in the : menu,
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
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains(": 菜单占用"));
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
