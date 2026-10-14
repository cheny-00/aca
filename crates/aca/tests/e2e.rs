//! 从命令行这一头整条跑一遍:起真的 tmux 会话(里面是个假 agent),发 hook,看状态、标题、事件。
//!
//! 每个测试自己一个 `ACA_HOME` 和一个 tmux 服务(`-L`),互不相干,也碰不到真的会话。
//! 机器上没有 tmux 就跳过。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Sandbox {
    home: PathBuf,
    socket: String,
}

/// 外层会话的身份变量一个不留:tmux 服务会把起它那一刻的环境带给每个会话
fn scrub(c: &mut Command) -> &mut Command {
    for (k, _) in std::env::vars() {
        // CLAUDE 开头的全拿:里面有外层会话用哪个配置目录、登录凭据去哪取
        if k.starts_with("AGENTDECK_") || k.starts_with("CLAUDE") || k.starts_with("ACA_") || matches!(k.as_str(), "TMUX" | "TMUX_PANE") {
            c.env_remove(k);
        }
    }
    // aca 开 worktree 时调 git:不读你的全局配置(hook、签名)
    c.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1");
    // 下面的断言按中文写(默认是英文)
    c.env("ACA_LANG", "zh")
}

/// 测试里自己调 git:同样不读全局配置,要成功,给标准输出
fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let o = scrub(&mut c).arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}:{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

impl Sandbox {
    /// None = 没装 tmux
    fn new(name: &str) -> Option<Sandbox> {
        if Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("没有 tmux,跳过");
            return None;
        }
        let home = std::env::temp_dir().join(format!("aca-e2e-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("config")).unwrap();
        std::fs::create_dir_all(home.join("proj")).unwrap();
        // 假 agent:按 Claude 那一家起(带着 --session-id、--settings),自己什么也不干
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

    /// 跑一条,要成功,给标准输出
    fn aca(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(o.status.success(), "aca {args:?} 失败:{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.aca(args)).unwrap()
    }

    /// agent 那边来了一个事件:和真的一样,JSON 进标准输入,是哪个会话看环境变量
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

    /// 等窗格上出现这段字(假 agent 起来要一点时间),返回整屏
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

    /// 屏幕由文件管的两个假 agent:`scr` 按 Claude 那一家、`fcx` 按 Codex 那一家起。`screens/<会话 id>` 里写什么,
    /// 它那一格就显示什么(在跑、在等你选、输入框里有草稿……都靠这个摆出来);被当成 `codex queue` 调的时候,
    /// 把收到的参数一行一个记进 queued.txt
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

    /// 摆好一个会话的屏幕;不是空的就等它画出来
    fn set_screen(&self, id: &str, pane: &str, text: &str) {
        write(&self.home.join("screens").join(id), text);
        // 屏幕上(不带颜色)应该出现的:第一行去掉颜色之后的头几个字
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

    /// 以某个会话的身份跑(在那个会话里调的),标准输入给一段
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

    /// 等到成真,最多 8 秒(后台送信的那些进程要一点时间)
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
        // 服务没了,套接字文件 tmux 不收。这时候问不到它了(测试里服务可能早就自己退了),按 tmux 的规矩拼出来
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
    // 会话名就是标题(`.` 换掉),身份记在 tmux 的选项上
    assert_eq!(sb.tmux(&["list-sessions", "-F", "#{session_name}|#{@aca_id}"]), format!("修登录 v1_2|{id}"));
    let row = sb.json(&["show", &id, "--json"]);
    assert_eq!((row["state"].as_str(), row["group"].as_str(), row["tmux"].as_str()), (Some("idle"), Some("work/app"), Some("修登录 v1_2")));
    let pane = row["pane"].as_str().unwrap().to_string();

    // 改名:tmux 会话名、窗口名一起变;id 写开头几位、写标题都找得到
    sb.aca(&["rename", &id[..4], "登录页改版"]);
    assert_eq!(sb.tmux(&["list-windows", "-a", "-F", "#{session_name}|#{window_name}"]), "登录页改版|登录页改版");
    assert_eq!(sb.json(&["show", "登录页改版", "--json"])["id"], id.as_str());

    // 停:agent 没了,窗格和会话都留着;再起还是那一格
    sb.aca(&["stop", &id]);
    assert_eq!(sb.json(&["show", &id, "--json"])["state"], "exited");
    assert_eq!(sb.tmux(&["list-panes", "-a", "-F", "#{pane_id}|#{pane_dead}"]), format!("{pane}|1"));
    sb.aca(&["start", &id]);
    assert_eq!(sb.tmux(&["list-panes", "-a", "-F", "#{pane_id}|#{pane_dead}"]), format!("{pane}|0"));

    // tmux 里整个没了(像重启过电脑):登记还在,再起就重新建
    sb.tmux(&["kill-server"]);
    assert_eq!(sb.json(&["show", &id, "--json"])["state"], "stopped");
    sb.aca(&["start", &id]);
    assert_eq!(sb.tmux(&["list-sessions", "-F", "#{session_name}|#{@aca_id}"]), format!("登录页改版|{id}"));

    sb.aca(&["rm", &id]);
    assert_eq!(sb.tmux(&["list-sessions", "-F", "#{session_name}"]), "");
    assert_eq!(sb.json(&["ls", "--json"]), serde_json::json!([]));
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
    // 假 agent 的屏幕上什么也没有,所以这里只看 hook 说的(--no-screen)
    let state = |sb: &Sandbox| sb.json(&["ls", "--json", "--no-screen"])[0]["state"].clone();
    assert_eq!(state(&sb), "running");
    hook("PermissionRequest", ",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"kubectl logs web-1\"}");
    let row = &sb.json(&["ls", "--json", "--no-screen"])[0];
    assert_eq!((row["state"].as_str(), row["ask"].as_str()), (Some("ask"), Some("Bash:kubectl logs web-1")));

    // 跑完时对话记录里已经有它自己起的标题:会话名跟着改,最后一条回复留着,没人看过所以是「没看」
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

    // 人改过名之后不再跟 agent 的标题;放开锁又跟回去
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
    // 不写 kind 的工具:假 agent 把收到的参数、aca 放进环境的工具名打在屏幕上,TOY_BUSY 有值时再打一行「在忙」
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
    // 屏幕上认出来在忙(配置里的正则)
    assert_eq!(sb.json(&["show", &id, "--json"])["state"], "running");

    // 转发脚本按协议报事件:`aca hook <工具名>`。对话 id、最后一条回复、它自己起的标题都接住了
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

    // 停了再起:照 resume 的模板接着那段对话
    sb.aca(&["stop", &id]);
    sb.aca(&["start", &id]);
    let screen = sb.wait_screen(&pane, "args:[--session t-1]");
    assert!(screen.contains("args:[--session t-1]"), "{screen}");
    // 没写 fork:分不了叉
    assert!(!sb.run(&["fork", &id]).status.success());
}

#[test]
fn an_account_is_a_layer_on_the_launch_and_run_hands_it_to_the_tools_own_commands() {
    let Some(sb) = Sandbox::new("account") else { return };
    // 假 agent 把账号那一层打在屏幕上(目录只打最后一段,免得折行);带了 login 就当成它自己的子命令,打完就退
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

    // 登记过的账号:目录、env_file 里的 key、参数都带上了,参数接在工具的后面
    let work = sb.aca(&["new", "toy:work", &sb.proj()]);
    let screen = screen_of(&work, "sess:[");
    assert!(screen.contains("args:[--yolo --profile w]") && screen.contains("home:[toy-work]") && screen.contains("key:[k-1]"), "{screen}");
    // 账号只带在 agent 那一格上:同一个 tmux 会话里另开的窗口(prefix c、以后放进来的别的 agent)一样都拿不到
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

    // 不写账号:aca 什么都不加
    let bare = sb.aca(&["new", "toy", &sb.proj()]);
    let screen = screen_of(&bare, "sess:[");
    assert!(screen.contains("args:[--yolo]") && screen.contains("home:[]") && screen.contains("key:[]"), "{screen}");

    // 没登记的目录直接用:记完整路径,列表里显示目录名;正好是登记过的那个目录,就叫它的名字
    let adhoc = sb.aca(&["new", &format!("toy:{}/", at("toy-new")), &sb.proj()]);
    assert!(screen_of(&adhoc, "sess:[").contains("home:[toy-new]"));
    assert_eq!(sb.json(&["show", &adhoc, "--json"])["account"], at("toy-new").as_str());
    assert!(sb.aca(&["ls", "--no-screen"]).contains("toy:toy-new"));
    let plain = sb.aca(&["new", &format!("toy:{}", at("toy-plain")), &sb.proj(), "--hold"]);
    assert_eq!(sb.json(&["show", &plain, "--json"])["account"], "plain");

    // 不建会话,带着账号原样跑它自己的命令:账号的环境在,参数只有你给的,会话的身份不带
    let o = sb.cmd().args(["run", "toy:work", "--", "login", "--device"]).env("ACA_SESSION_ID", &work).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "args:[login --device]\nhome:[toy-work]\nkey:[k-1]\nsess:[]\n");
    assert_eq!(sb.json(&["ls", "--json", "--no-screen"]).as_array().unwrap().len(), 4);

    // 写坏了的配置:命令行说一声坏在哪
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
    // 事件钩子:回头调 aca 往这一行上挂一段字,再留个记号说自己跑过了
    write(
        &dir.join("mark.sh"),
        "\"$ACA_BIN\" meta set \"$ACA_SESSION_ID\" mark \"$ACA_PLUGIN_EVENT\"\ntouch \"$ACA_PLUGIN_STATE_DIR/ran\"\n",
    );
    assert!(sb.aca(&["plugin", "link", &dir.to_string_lossy()]).contains("test.mark"));

    let id = sb.aca(&["new", "fake", &sb.proj()]);
    let pane = sb.json(&["show", &id, "--json"])["pane"].as_str().unwrap().to_string();
    sb.hook(&id, &pane, "{\"hook_event_name\":\"Stop\",\"session_id\":\"c1\",\"last_assistant_message\":\"好\"}");
    // 钩子是不等的:给它一点时间
    let ran = sb.home.join("state/plugins/test.mark/ran");
    for _ in 0..50 {
        if ran.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ran.exists(), "插件的事件钩子没跑:{}", sb.aca(&["plugin", "log", "test.mark"]));
    assert_eq!(sb.json(&["show", &id, "--json"])["tokens"]["mark"], "turn.done");

    // 动作在前台跑,拿得到是冲着哪个会话的
    assert_eq!(sb.aca(&["plugin", "run", "test.mark.hello", "--session", &id]), format!("hi {id} from test.mark"));
    // 关掉之后不再收事件、动作也调不了
    sb.aca(&["plugin", "disable", "test.mark"]);
    assert!(!sb.run(&["plugin", "run", "test.mark.hello"]).status.success());
}

/// 一个不是 aca 起的「Claude」:进程名是 claude、什么参数都收、一直挂着;外面包着一层 shell(agent-deck 起的就是这个样子),
/// 带着自己的配置目录。返回 (tmux 会话号, 窗格, agent 的 pid);这台机器编不出那个假程序 = None。
/// 得是个真叫这个名字的程序:系统认进程名看的是文件本身,链接、脚本都骗不过;系统自带的程序拷出来又不让跑。所以现编一个
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

/// 接管要读 `~` 底下的东西(默认的配置目录、agent-deck 的库):把 HOME 也指进沙盒
fn at_home(sb: &Sandbox, args: &[&str]) -> String {
    let o = sb.cmd().env("HOME", &sb.home).args(args).output().unwrap();
    assert!(o.status.success(), "aca {args:?} 失败:{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn a_running_session_aca_did_not_start_is_adopted_in_place_restarted_and_released() {
    let Some(sb) = Sandbox::new("adopt") else { return };
    // 重启之后 aca 起的也是那个假的 claude(什么参数都收);起成了什么样,看进程的命令行
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
    // 真的 Claude 自己记的那份:现在是哪段对话、标题;对话记录在它的配置目录里
    write(
        &sb.home.join(format!("cc/sessions/{pid}.json")),
        &format!(r#"{{"pid":{pid},"sessionId":"c-live","cwd":"{}","name":"修登录"}}"#, sb.proj()),
    );
    write(&sb.home.join("cc/projects/p/c-live.jsonl"), "{\"type\":\"user\"}\n");
    let aca = |args: &[&str]| at_home(&sb, args);
    let ls = || serde_json::from_str::<Value>(&aca(&["ls", "--json"])).unwrap();
    let panes = || sb.tmux(&["list-panes", "-a", "-F", "#{session_name}|#{@aca_id}|#{@aca_agent}|#{remain-on-exit}|#{pane_dead}"]);

    // 不带参数:列出来看,什么都不动。认出了哪一家、哪个账号、哪段对话、要照搬的参数
    let listed = aca(&["adopt"]);
    assert!(
        listed.contains("「修登录」 claude:sub") && listed.contains("对话 c-live") && listed.contains("带 --dangerously-skip-permissions"),
        "{listed}"
    );
    assert!(aca(&["adopt", &sid, "-n"]).contains("会接管"));
    assert_eq!(ls(), serde_json::json!([]));
    assert_eq!(panes(), format!("{tmux_name}|||off|0"));

    // 接管:登记了、tmux 会话名改成标题、agent 那一格记上了;进程还是原来那个,没被动过
    let said = aca(&["adopt", &tmux_name, "-g", "work"]);
    assert!(said.contains("接管了「修登录」") && said.contains(&format!("{tmux_name} → 修登录")), "{said}");
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
    // 照搬的参数里没有「接哪段对话」的那两个词
    assert!(aca(&["show", &id, "--json"]).contains("\"args\":[\"--dangerously-skip-permissions\"]"));
    assert!(alive(&pid), "接管不该动那个进程");
    // 再接一次:已经是 aca 的了
    let again = sb.cmd().env("HOME", &sb.home).args(["adopt", "修登录"]).output().unwrap();
    assert!(!again.status.success() && String::from_utf8_lossy(&again.stderr).contains("已经是 aca 的会话"));

    // 重启:原来那个进程退干净了(它不是窗格的根进程,外面还有一层 shell),在同一格里由 aca 起:接着那段对话、
    // 带着 hook、照搬的参数还在
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

    // 放手:登记没了,tmux 会话和里面的进程照留,变回普通会话(remain-on-exit 也收回去)
    assert!(aca(&["release", &id]).contains("放手了「修登录」"));
    assert_eq!(ls(), serde_json::json!([]));
    assert_eq!(panes(), "修登录|||off|0");
    assert!(alive(&now));

    // 再接回来:这个进程是 aca 起的,它的 hook 还在往原来那个 id 上报 —— 沿用那个 id;账号从它的环境里认;
    // aca 自己带进去的那几样(--resume、它的 --settings)不当成要照搬的参数
    write(&sb.home.join(format!("cc/sessions/{now}.json")), &format!(r#"{{"pid":{now},"sessionId":"c-live","cwd":"{}"}}"#, sb.proj()));
    let said = aca(&["adopt", &pane]);
    assert!(said.contains(&format!("接管了「修登录」 {id}")) && !said.contains("重启之前"), "{said}");
    let row = ls()[0].clone();
    assert_eq!((row["id"].as_str(), row["account"].as_str(), row["conv"].as_str()), (Some(id.as_str()), Some("sub"), Some("c-live")));
    assert!(aca(&["show", &id, "--json"]).contains("\"args\":[\"--dangerously-skip-permissions\"]"));

    // 认不出的程序(一个 sleep):照 --as 说的登记;标题、对话、参数自己给
    let other = sb.tmux(&["new-session", "-d", "-P", "-F", "#{pane_id}", "-s", "杂项", "-c", &sb.proj(), "sleep 600"]);
    assert!(aca(&["adopt", "杂项", "-n"]).contains("shell"));
    let said = aca(&["adopt", &other, "--as", "fake:sub", "-t", "又一个", "--conv", "c-9", "--", "--mine"]);
    assert!(said.contains("接管了「又一个」"), "{said}");
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
    // 真的 Claude 没起名字时自己编一个占位的(nameSource 是 derived):那不是标题。标题它写在窗格标题上
    write(
        &sb.home.join(format!("cc/sessions/{pid}.json")),
        &format!(r#"{{"pid":{pid},"sessionId":"c-live","cwd":"{}","name":"proj-5e","nameSource":"derived"}}"#, sb.proj()),
    );
    let on_pane = |pane: &str, title: &str| sb.tmux(&["select-pane", "-t", pane, "-T", title]);
    on_pane(&pane, "✳ 登录页 500 排查");
    let aca = |args: &[&str]| at_home(&sb, args);
    let said = aca(&["adopt", &pane]);
    assert!(said.contains("接管了「登录页 500 排查」"), "{said}");
    let row = |id: &str| {
        let all = serde_json::from_str::<Value>(&aca(&["ls", "--json", "--no-screen"])).unwrap();
        all.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone()
    };
    let names = |id: &str| {
        let r = row(id);
        (r["title"].as_str().unwrap().to_string(), r["tmux"].as_str().unwrap().to_string())
    };
    let id = serde_json::from_str::<Value>(&aca(&["ls", "--json", "--no-screen"])).unwrap()[0]["id"].as_str().unwrap().to_string();

    // 接管了没重启,没带 hook:它换了标题,列一次就跟上,tmux 会话名也改
    on_pane(&pane, "✳ 换了个标题");
    assert_eq!(names(&id), ("换了个标题".into(), "换了个标题".into()));
    // 新开一段对话时它写回「还没起标题」的那个:不算,留着原来的
    on_pane(&pane, "✳ Claude Code");
    assert_eq!(names(&id).0, "换了个标题");
    // 人改过名的不跟,放开又跟回去
    aca(&["rename", &id, "我起的名字"]);
    on_pane(&pane, "◐ 又换了");
    assert_eq!(names(&id), ("我起的名字".into(), "我起的名字".into()));
    aca(&["rename", &id, "--auto"]);
    assert_eq!(names(&id), ("又换了".into(), "又换了".into()));

    // aca 自己起的也一样:这一轮还没跑完,标题已经在窗格上了,不用等跑完的那个事件
    let own = aca(&["new", "fake", &sb.proj()]);
    let own_pane = row(&own)["pane"].as_str().unwrap().to_string();
    assert_eq!(names(&own).0, "proj");
    on_pane(&own_pane, "✳ 第一轮就有的标题");
    assert_eq!(names(&own), ("第一轮就有的标题".into(), "第一轮就有的标题".into()));
    // 跑完时对话记录里写的是另一个:听窗格上的(它自己显示的就是那个),不来回改
    let transcript = sb.home.join("c1.jsonl");
    write(&transcript, "{\"type\":\"ai-title\",\"aiTitle\":\"记录里的\",\"sessionId\":\"c1\"}\n");
    sb.hook(
        &own,
        &own_pane,
        &format!("{{\"hook_event_name\":\"Stop\",\"session_id\":\"c1\",\"transcript_path\":\"{}\"}}", transcript.display()),
    );
    assert_eq!(names(&own), ("第一轮就有的标题".into(), "第一轮就有的标题".into()));
    // 事件只在真跟着改了的那两次发:占位的、锁着的、没变的都不发
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
    // agent-deck 的库:只放接管要读的那几列
    let db = sb.home.join(".local/share/agent-deck/profiles/default/state.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let sql = "create table instances(id text, title text, title_locked integer, group_path text, sort_order integer, tool text, account text, \
               project_path text, tool_data text, tmux_session text, archived_at integer, parent_session_id text, created_at integer); \
               insert into instances values('ad-77', '架构替换方案', 0, 'runtime', 4, 'claude', 'sub', '/p', \
               '{\"claude_session_id\":\"c-deck\"}', 'agentdeck_iron-dune_92cb', 0, '', 1790000000);";
    assert!(Command::new("sqlite3").arg(&db).arg(sql).status().unwrap().success());
    // 它起的会话:会话名是它编的,进程环境里带着它的 id。这个假的没留「现在是哪段对话」的文件:认命令行里的那个
    let Some((_, pane, pid)) = foreign_claude(&sb, Some("agentdeck_iron-dune_92cb"), "AGENTDECK_INSTANCE_ID=ad-77") else { return };
    let aca = |args: &[&str]| at_home(&sb, args);

    // 写名字里的一段就行
    let said = aca(&["adopt", "iron-dune"]);
    assert!(said.contains("接管了「架构替换方案」 ad-77") && said.contains("agent-deck -p default remove ad-77"), "{said}");
    let row = serde_json::from_str::<Value>(&aca(&["show", "ad-77", "--json"])).unwrap();
    assert_eq!(
        (row["title"].as_str(), row["group"].as_str(), row["conv"].as_str(), row["tmux"].as_str(), row["created"].as_u64()),
        (Some("架构替换方案"), Some("runtime"), Some("c-old"), Some("架构替换方案"), Some(1_790_000_000))
    );
    // 没登记过的配置目录:账号直接记那个目录
    assert_eq!(row["account"].as_str(), sb.home.join("cc").to_str());
    // 库只读:那边的记录原样
    let o = Command::new("sqlite3").arg(&db).arg("select tmux_session from instances").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "agentdeck_iron-dune_92cb");

    // 重启之前它没带 aca 的 hook,但带着 agent-deck 的、照常在报:那边记的借过来,不用等重启就有「在跑」「没看」
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

    // 停:里面的 agent 和外面那层 shell 都退了,窗格留着
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

    // 先整批搬过(连在跑的也登记了):登记着,在 aca 看来是停着的
    assert!(aca(&["import", "agent-deck", "--apply", "--all"]).contains("搬了 1 个"));
    assert_eq!(ls()[0]["state"], "stopped");
    let Some((_, pane, _)) = foreign_claude(&sb, Some("agentdeck_k_5"), "AGENTDECK_INSTANCE_ID=ad-5") else { return };

    // 接管:不另登记一条,把这个 tmux 会话接到那条上。对话、参数以进程上现在的为准,别的照登记的
    let said = aca(&["adopt", "k_5"]);
    assert!(said.contains("接管了「导入过的」 ad-5"), "{said}");
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

    // 又来两个没登记的:--all 只接认得出 agent 的那个,已经是 aca 的不重复接
    foreign_claude(&sb, None, "").unwrap();
    sb.tmux(&["new-session", "-d", "-s", "只是个壳", "-c", &sb.proj(), "sleep 600"]);
    let listed = aca(&["adopt"]);
    assert!(listed.contains("另有 1 个 tmux 会话没认出 agent") && listed.contains("只是个壳") && !listed.contains("导入过的"), "{listed}");
    let said = aca(&["adopt", "--all", "-g", "批量"]);
    assert_eq!(said.matches("接管了").count(), 1, "{said}");
    let rows = ls();
    let groups: Vec<&str> = rows.as_array().unwrap().iter().map(|r| r["group"].as_str().unwrap()).collect();
    assert_eq!(groups, ["g", "批量"]);
    assert_eq!(aca(&["adopt", "--all"]), "没有能直接接管的 tmux 会话");
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
    // 目录登记成分组:名字是目录名,再来一次还是它
    assert_eq!(sb.aca(&["group", "add", &a]), "aca");
    assert_eq!(sb.aca(&["group", "add", &a]), "aca");
    // 同名的记着别的目录:往后编号。new -g . 进按目录定的组,目录记成它的默认目录
    let id = sb.aca(&["new", "fake", &b, "--hold", "-g", "."]);
    assert_eq!(sb.json(&["show", &id, "--json"])["group"], "aca-2");
    assert_eq!(sb.aca(&["group", "dir", "aca-2"]), b);

    // 改名(换个上级):会话跟过去,界面记着它的(工作区、在看的组、折着的)一起改
    let ui = sb.home.join("state/ui");
    write(&ui.join("spaces"), "group\tdefault\taca-2\th\ngroup\tdefault\taca\t2\n");
    write(&ui.join("view"), "group=aca-2\nfolded=aca-2\tother\n");
    assert_eq!(sb.aca(&["group", "rename", "aca-2", "lab/aca"]), "aca-2 → lab/aca,1 个会话跟过去");
    assert_eq!(sb.json(&["show", &id, "--json"])["group"], "lab/aca");
    let spaces = std::fs::read_to_string(ui.join("spaces")).unwrap();
    assert!(spaces.contains("group\tdefault\tlab/aca\th\n") && spaces.contains("group\tdefault\taca\t2\n"), "{spaces}");
    let view = std::fs::read_to_string(ui.join("view")).unwrap();
    assert!(view.contains("group=lab/aca\n") && view.contains("folded=lab/aca\tother\n"), "{view}");
    // 撞上已有的:不改
    let o = sb.run(&["group", "rename", "lab/aca", "ACA"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("已经有 aca"));

    // 默认目录:换一个;是别的组的不收;'' = 不记
    sb.aca(&["group", "dir", "lab/aca", &c]);
    assert_eq!(sb.aca(&["group", "dir", "lab/aca"]), c);
    let o = sb.run(&["group", "dir", "lab/aca", &a]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("分组 aca"));
    sb.aca(&["group", "dir", "lab/aca", ""]);
    assert_eq!(sb.aca(&["group", "dir", "lab/aca"]), "");
    // move .:b 不再是谁的默认目录了,按名字重新定(aca 记着别的目录 → aca-2),说一声进了哪个
    assert_eq!(sb.aca(&["move", &id, "."]), "aca-2");
    assert_eq!(sb.aca(&["group", "dir", "aca-2"]), b);
    // 上一级目录名是已有的分组:嵌在它下面(和界面的 N 是同一套),目录记成新组的默认目录
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
    // 假的 claude:`plugin validate` 时把 report.json 打出来(真的那条是让这一版自己验 mod),别的时候是假 agent,把收到的参数记下。
    // 每「装」一版文件就变(多一个 #),结论按版本重新下
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

    // 这一版验得过:mod 和 hook 一样是启动时带的,文件在 aca 自己的状态目录里
    install(1, passes);
    let (one, argv) = start();
    assert!(argv.contains(&carried) && argv.contains("--settings"), "{argv}");
    assert!(mods.join("hooks/register.js").is_file() && mods.join(".claude-plugin/plugin.json").is_file());
    // 它交来一次用量:记在这个会话自己的文件里,账号是它环境里的配置目录
    let measure = r#"{"context":{"window":1000000},"rateLimits":[{"kind":"five_hour","percentUsed":6,"resetsAt":"2026-10-09T11:40:00.000Z"}],"cost":{"usd":0},"changed":["rateLimits"]}"#;
    hands_over(&one, measure);
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(state.join(format!("ui/limits/mod-{one}.json"))).unwrap()).unwrap();
    assert_eq!(rec["dir"].as_str().unwrap(), sb.home.join("acct").to_string_lossy());
    assert_eq!((rec["wins"][0]["label"].as_str(), rec["wins"][0]["used"].as_f64()), (Some("5h"), Some(6.0)));
    assert!(state.join(format!("mod/heard/{one}")).is_file());
    // 交来的东西认不出了(事件的形状变了):这一版之后不带
    hands_over(&one, r#"{"context":{"window":1000000},"limits":[]}"#);
    let (two, argv) = start();
    assert!(!argv.contains("--plugin-dir"), "{argv}");
    assert!(whys().contains("认不出") && !state.join(format!("mod/up/{two}")).exists());

    // 换了一版,它自己说不认这个事件:不带
    install(
        2,
        r#"{"success":false,"manifest":{"errors":[]},"contents":[{"errors":[{"path":"modules../register.js","message":"aca: /x/register.js:5: \"session.measure\" is not an event; $ is always spelled $.noun.event(...)"}],"notes":[]}]}"#,
    );
    assert!(!start().1.contains("--plugin-dir"));
    assert!(whys().contains("is not an event"));

    // 又一版,验得过、带上了,但跑完一轮一次都没交来(`$` 上的调用改了名,它验不出):下次起就不带了
    install(3, passes);
    let (four, argv) = start();
    assert!(argv.contains(&carried), "{argv}");
    // 起了一分钟了(把记的时刻往回拨),跑完一轮
    let up = state.join(format!("mod/up/{four}"));
    let (key, at) =
        std::fs::read_to_string(&up).unwrap().trim().split_once(' ').map(|(k, t)| (k.to_string(), t.parse::<u64>().unwrap())).unwrap();
    write(&up, &format!("{key} {}\n", at - 60));
    let pane = sb.json(&["show", &four, "--json"])["pane"].as_str().unwrap().to_string();
    sb.hook(&four, &pane, r#"{"hook_event_name":"UserPromptSubmit","session_id":"c4","prompt":"hi"}"#);
    sb.hook(&four, &pane, r#"{"hook_event_name":"Stop","session_id":"c4"}"#);
    assert!(!start().1.contains("--plugin-dir"));
    assert!(whys().contains("没交来"), "{}", whys());
}

#[test]
fn a_finished_worker_rings_its_parents_doorbell_and_claude_takes_the_notice_itself() {
    let Some(sb) = Sandbox::new("bell") else { return };
    let lead = sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    // 没说要通知的:跑完也不叫醒谁
    let quiet = sb.aca(&["new", "fake", &sb.proj(), "-t", "安静", "--parent", &lead]);
    sb.hook(&quiet, &sb.pane(&quiet), r#"{"hook_event_name":"Stop","session_id":"c9","last_assistant_message":"好"}"#);
    assert_eq!(sb.json(&["inbox", &lead, "--json"]), serde_json::json!([]));
    let w = sb.aca(&["new", "fake", &sb.proj(), "-t", "读文档", "--parent", &lead, "--notify", "all"]);
    // 主线的 Claude 起来了:SessionStart 多跑的那一条回一个要它盯着的文件(它的门铃),标准输出只有这一段
    let o = sb.as_session(&lead, &["inbox", "watch"], r#"{"hook_event_name":"SessionStart","source":"startup"}"#);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let bell = v["hookSpecificOutput"]["watchPaths"][0].as_str().unwrap().to_string();
    assert!(bell.ends_with(&format!("inbox/{lead}.bell")), "{v}");
    let before = std::fs::read_to_string(&bell).unwrap();

    // worker 跑完一轮:它自己的 hook 往主线的收件箱放一条通知,再按一下门铃
    sb.hook(&w, &sb.pane(&w), r#"{"hook_event_name":"Stop","session_id":"c1","last_assistant_message":"整理好了,\n结论写在 notes.md"}"#);
    assert!(sb.until(|| std::fs::read_to_string(&bell).unwrap() != before), "门铃没响");
    let inbox = sb.json(&["inbox", &lead, "--json"]);
    assert_eq!(
        (inbox[0]["kind"].as_str(), inbox[0]["what"].as_str(), inbox[0]["from"].as_str(), inbox[0]["stage"].as_str()),
        (Some("notice"), Some("turn.done"), Some(w.as_str()), Some("pending"))
    );

    // Claude 看到门铃变了,在后台跑取信的那条 hook:写到 stderr、退出码 2,它就把这段当成系统提示给模型
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
    // 取过了:再响一次什么都不发生
    assert_eq!(take().status.code(), Some(0));
    assert_eq!(sb.json(&["inbox", &lead, "--json"])[0]["stage"], "got");

    // 你自己开的会话(没有 parent)跑完了,不通知谁
    sb.hook(&lead, &sb.pane(&lead), r#"{"hook_event_name":"Stop","session_id":"c0","last_assistant_message":"好"}"#);
    let names = sb.event_names();
    assert_eq!(names.iter().filter(|n| *n == "message.sent").count(), 1, "{names:?}");
    assert_eq!(names.iter().filter(|n| *n == "message.delivered").count(), 1, "{names:?}");
    // 删掉会话,收件箱一起没了
    sb.aca(&["rm", &lead]);
    assert!(!Path::new(&bell).exists());
}

/// 分叉出来的是你自己接着聊的另一条路:和原会话平级(没有 parent,分自谁在 fork_of),排在原会话那一家的紧后面;
/// 它跑完不通知原会话,原会话也不能替它答
#[test]
fn a_fork_is_a_peer_listed_right_after_the_original_and_not_one_it_dispatched() {
    let Some(sb) = Sandbox::new("fork-peer") else { return };
    // 配置里让 worker 跑完都通知派它的那个:分叉出来的不该跟着通知
    let cfg = sb.home.join("config/config.toml");
    write(&cfg, &format!("{}[inbox]\nnotify = [\"done\"]\n", std::fs::read_to_string(&cfg).unwrap()));
    let lead = sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    let worker = sb.aca(&["new", "fake", &sb.proj(), "-t", "读文档", "--parent", &lead]);
    let other = sb.aca(&["new", "fake", &sb.proj(), "-t", "别的事"]);
    let fork = sb.aca(&["fork", &lead]);
    // worker 的分叉:也是顶格的,排到派 worker 的那个后面
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

    // 事件里:分自谁,没有 parent
    let made: Vec<Value> = sb.aca(&["events", "-n", "100"]).lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
    let made = made.iter().find(|e| e["event"] == "session.created" && e["session"] == fork.as_str()).unwrap();
    assert_eq!((made["fork"].as_bool(), made["fork_of"].as_str(), made["parent"].as_str()), (Some(true), Some(lead.as_str()), Some("")));

    // 分叉跑完一轮:主线的收件箱里没有它;worker 跑完的照样通知
    sb.hook(&fork, &sb.pane(&fork), r#"{"hook_event_name":"Stop","session_id":"f1","last_assistant_message":"好"}"#);
    assert_eq!(sb.json(&["inbox", &lead, "--json"]), serde_json::json!([]));
    sb.hook(&worker, &sb.pane(&worker), r#"{"hook_event_name":"Stop","session_id":"w1","last_assistant_message":"读完了"}"#);
    assert_eq!(sb.json(&["inbox", &lead, "--json"])[0]["from"], worker.as_str());
    // 分叉里发给 parent:它没有
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

    // 还没说过话:没有对话 id 排不进去;屏幕上也没有输入框,不打字。先存着
    let said = sb.aca(&["msg", &cx, "先看看 README"]);
    assert!(said.contains("先存着"), "{said}");
    assert_eq!(sb.queued(), "");
    // 它说过一句、这一轮跑完了:它自己的 Stop hook 把存着的排进它的队列
    hook(r#"{"hook_event_name":"SessionStart","session_id":"th-1","source":"startup"}"#);
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"第一句"}"#);
    hook(r#"{"hook_event_name":"Stop","session_id":"th-1","last_assistant_message":"好"}"#);
    assert!(sb.until(|| sb.queued().contains("先看看 README")), "没排进去:{}", sb.queued());
    // 带一个 -c(在自己进程里写完就走,不拉起后台服务)、对话 id、那段话
    let q = sb.queued();
    let args: Vec<&str> = q.lines().collect();
    assert_eq!(args[..6], ["-c", "notify=[]", "queue", "--thread", "th-1", "--message"]);
    // 你发的:原样排进去,不加「别的会话转来的」(Codex 会当成注入)
    assert_eq!(args[6], "先看看 README", "{q}");
    assert!(sb.until(|| stage(0) == "queued"), "{}", stage(0));
    // Codex 为这段开了一轮:UserPromptSubmit 里带着原话,记成送到了
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"先看看 README"}"#);
    assert_eq!(stage(0), "got");

    // 它在跑:再发的先存着,这一轮跑完马上排进去
    sb.set_screen(&cx, &pane, "• Working (5s • esc to interrupt)");
    let said = sb.aca(&["msg", &cx, "再补一句"]);
    assert!(said.contains("它在跑"), "{said}");
    sb.set_screen(&cx, &pane, "• 好了");
    hook(r#"{"hook_event_name":"Stop","session_id":"th-1","last_assistant_message":"好了"}"#);
    assert!(sb.until(|| sb.queued().contains("再补一句")), "{}", sb.queued());

    // 你刚按 Esc 打断了它:不去催,等你下一轮跑完
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"写一篇长文"}"#);
    hook(r#"{"hook_event_name":"Interrupt","session_id":"th-1"}"#);
    let said = sb.aca(&["msg", &cx, "第三句"]);
    assert!(said.contains("打断"), "{said}");
    assert!(!sb.queued().contains("第三句"));
}

#[test]
fn without_a_doorbell_a_message_is_typed_in_only_when_the_input_box_is_empty() {
    let Some(sb) = Sandbox::new("type") else { return };
    sb.screen_tools();
    // 按 Claude 起的,但这次没接上门铃(像接管来、还没重启的):只能打字
    let id = sb.aca(&["new", "scr", &sb.proj()]);
    let pane = sb.pane(&id);
    // 输入框里有你没发的字:不打,过一阵再看
    sb.set_screen(&id, &pane, "❯ 人打的草稿");
    let said = sb.aca(&["msg", &id, "补一下测试"]);
    assert!(said.contains("输入框里有你没发的字"), "{said}");
    // 只有灰色的提示:打进去,存着的那条一起
    sb.set_screen(&id, &pane, "❯ \x1b[2mTry \"fix typecheck errors\"\x1b[0m");
    let said = sb.aca(&["msg", &id, "跑一下 e2e"]);
    assert!(said.contains("打字送进去了"), "{said}");
    let screen = sb.wait_screen(&pane, "跑一下 e2e");
    assert!(screen.contains("补一下测试") && screen.contains("跑一下 e2e") && !screen.contains("[aca]"), "{screen}");
    let inbox = sb.json(&["inbox", &id, "--json"]);
    assert!(inbox.as_array().unwrap().iter().all(|m| m["stage"] == "got"), "{inbox}");
    // shell 会话不收:打进去就成了一条命令
    let sh = sb.aca(&["new", "shell", &sb.proj()]);
    assert!(sb.aca(&["msg", &sh, "ls"]).contains("shell"));
    // 没有这个会话、没给话
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
    // 它用选择题问了一句(2.1.294 的样子)
    sb.set_screen(&w, &pane, " ☐ 测试选择\n测试选 A 还是 B？\n❯ 1. A\n  2. B\nEnter to select · ↑/↓ to navigate · Esc to cancel");
    sb.hook(
        &w,
        &pane,
        r#"{"hook_event_name":"PreToolUse","session_id":"c1","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"测试选 A 还是 B？","multiSelect":false,"options":[{"label":"A"},{"label":"B"}]}]}}"#,
    );
    // 派它的那个收到通知:问的什么、有哪几个选项、怎么答
    let notice = sb.json(&["inbox", &lead, "--json"])[0]["text"].as_str().unwrap().to_string();
    assert_eq!(notice, format!("在问:「测试选 A 还是 B？」\n选项:1 A  2 B  回答:aca answer {w} <选项号>"));
    let shown = sb.aca(&["answer", &w]);
    assert!(shown.contains("  1 A\n  2 B") && shown.contains("aca answer"), "{shown}");

    // 普通的 send 不往确认框里打
    let o = sb.run(&["send", &w, "-m", "hi"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("aca answer"));
    // 别的会话不能替它答;选项号要对得上
    let o = sb.as_session(&other, &["answer", &w, "2"], "");
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("不是你派出去的"));
    assert!(!sb.as_session(&lead, &["answer", &w, "3"], "").status.success());
    // 派它的那个能:按下 2(假 agent 不读键,终端把它回显在屏幕上)
    let o = sb.as_session(&lead, &["answer", &w, "2"], "");
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("2 B"), "{}", String::from_utf8_lossy(&o.stderr));
    let screen = sb.wait_screen(&pane, "cancel\n2");
    assert!(screen.ends_with("Esc to cancel\n2"), "{screen}");
    let answered = sb.aca(&["events", "-n", "100"]).lines().find(|l| l.contains("ask.answered")).unwrap().to_string();
    assert!(answered.contains(&format!("\"by\":\"{lead}\"")) && answered.contains("\"answer\":\"2 B\""), "{answered}");
    // 答完了:问的那句清掉了
    assert_eq!(sb.json(&["show", &w, "--json"])["run"]["ask"], "");
}

/// 启动时的关卡(Codex 的信任框)只在屏幕上:列出编号的选项,答的时候用方向键走过去再回车
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
        shown.contains("在问:Folder access · /p/api · Trust this folder?") && shown.contains("  1 Trust and continue\n  2 Quit"),
        "{shown}"
    );
    assert!(!sb.as_session(&lead, &["answer", &w, "3"], "").status.success());
    // 选 2:往下一格再回车(假 agent 不读键,终端把方向键回显成 ^[[B)
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

    // 主线挂着等它
    let mut child =
        sb.cmd().args(["wait", &w, "--timeout", "20"]).env("ACA_SESSION_ID", &lead).stdout(std::process::Stdio::piped()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(child.try_wait().unwrap().is_none(), "它还在跑,不该等到");
    sb.set_screen(&w, &pane, "❯ ");
    sb.hook(&w, &pane, r#"{"hook_event_name":"Stop","session_id":"c1","last_assistant_message":"读完了"}"#);
    let o = child.wait_with_output().unwrap();
    assert_eq!((o.status.code(), String::from_utf8_lossy(&o.stdout).trim()), (Some(0), format!("{w} 跑完一轮:「读完了」").as_str()));
    // 它的通知已经记成送到了:门铃响了也不会再为它叫醒一次
    assert_eq!(sb.json(&["inbox", &lead, "--json"])[0]["stage"], "got");
    assert_eq!(sb.as_session(&lead, &["inbox", "take"], "{}").status.code(), Some(0));

    // 一直没开始干活的:到时间就回,退出码 124
    let idle = sb.aca(&["new", "scr", &sb.proj()]);
    let o = sb.run(&["wait", &idle, "--timeout", "1"]);
    assert_eq!((o.status.code(), String::from_utf8_lossy(&o.stdout).trim()), (Some(124), format!("{idle} 还没完(到时间了)").as_str()));
}

#[test]
fn a_plugin_installed_from_git_brings_its_claude_plugin_while_enabled_and_goes_away_cleanly() {
    let Some(sb) = Sandbox::new("plugins") else { return };
    // 一个插件仓库:清单里说要给 Claude 带 claude/ 这个插件目录
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
    // 按 Claude 起、把参数打在屏幕上的假 agent
    let cfg = sb.home.join("config/config.toml");
    std::fs::write(sb.home.join("args.sh"), "#!/bin/sh\necho \"args:[$*]\"\nexec sleep 600\n").unwrap();
    let text = std::fs::read_to_string(&cfg).unwrap()
        + &format!("[tools.argc]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", sb.home.join("args.sh").display());
    std::fs::write(&cfg, text).unwrap();
    // 窗格 80 列,长参数会折行:接回去再看
    let args_of = |id: &str| sb.wait_screen(&sb.pane(id), "]").replace('\n', "");

    // 从 git 装:克隆到 aca 的数据目录,记下从哪来的,带给 Claude
    let url = format!("file://{}", src.display());
    assert!(sb.aca(&["plugin", "install", &url]).contains("装好了 t.deleg(Deleg 0.1.0)"));
    let dir = sb.home.join("data/plugins/t.deleg");
    assert!(dir.join("claude/commands/hello.md").is_file());
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("● t.deleg  Deleg 0.1.0") && ls.contains(&format!("装自 {url}")) && ls.contains("带给 claude"), "{ls}");
    // 开着:起 Claude 时带上它的插件目录(只对这一次启动)
    let a = sb.aca(&["new", "argc", &sb.proj()]);
    let want = format!("--plugin-dir {}", dir.join("claude").canonicalize().unwrap().display());
    assert!(args_of(&a).contains(&want), "{}", args_of(&a));
    // 停着:新起的不带
    sb.aca(&["plugin", "disable", "t.deleg"]);
    let b = sb.aca(&["new", "argc", &sb.proj()]);
    assert!(!args_of(&b).contains("--plugin-dir"), "{}", args_of(&b));
    sb.aca(&["plugin", "enable", "t.deleg"]);
    // 同一个再装一次:不覆盖
    assert!(!sb.run(&["plugin", "install", &url]).status.success());
    // 仓库里出了新版本:update 拉过来,清单重新读
    std::fs::write(src.join("aca-plugin.toml"), manifest("0.2.0")).unwrap();
    git(&src, &["commit", "-q", "-am", "v2"]);
    let said = sb.aca(&["plugin", "update"]);
    assert!(said.contains("t.deleg:") && said.contains("→") && said.contains("Deleg 0.2.0"), "{said}");
    assert!(sb.aca(&["plugin", "update", "t.deleg"]).contains("已经是最新的"));
    // 删:连目录一起删,登记没了;插件自己的配置留着(aca 替它建的目录要是空的就不留,见 aca.copy 那条测试)
    write(&sb.home.join("config/plugins/t.deleg/mine.toml"), "x = 1\n");
    assert!(sb.aca(&["plugin", "remove", "t.deleg"]).contains("删了 t.deleg"));
    // 一个插件都不列了;起来时带着它的那个会话被点名:要重启才去掉
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(!dir.exists() && !ls.lines().any(|l| l.starts_with(['●', '○'])) && ls.contains("重启"), "{ls}");
    assert!(sb.home.join("config/plugins/t.deleg/mine.toml").is_file());
    // 钉在一个版本上:update 不动它
    sb.aca(&["plugin", "install", &url, "--ref", &v1]);
    assert!(sb.aca(&["plugin", "update", "t.deleg"]).contains("钉在"));
    assert!(sb.aca(&["plugin", "ls"]).contains("Deleg 0.1.0"));
    sb.aca(&["plugin", "remove", "t.deleg"]);
    // 本地目录:就是链接,删的时候目录不动
    assert!(sb.aca(&["plugin", "install", &src.to_string_lossy()]).contains("装好了 t.deleg"));
    assert!(sb.aca(&["plugin", "remove", "t.deleg"]).contains("没动"));
    assert!(src.join("aca-plugin.toml").is_file());
}

#[test]
fn a_plugin_whose_auto_setting_is_on_gives_claude_its_commands_without_the_only_when_typed_line() {
    let Some(sb) = Sandbox::new("auto") else { return };
    // 清单:auto 这个开关设置管 claude/ 里的命令 agent 自己能不能用;命令文件写着「只有你敲才进上下文」
    let dir = sb.home.join("autop");
    write(
        &dir.join("aca-plugin.toml"),
        "id = \"t.auto\"\nname = \"Auto\"\nversion = \"1\"\n[settings.auto]\ndefault = true\n[agent]\nfor = [\"claude\"]\ndir = \"claude\"\nauto = \"auto\"\n",
    );
    write(&dir.join("claude/.claude-plugin/plugin.json"), r#"{"name":"auto"}"#);
    let cmd = "---\ndescription: go\ndisable-model-invocation: true\n---\ngo $ARGUMENTS\n";
    write(&dir.join("claude/commands/go.md"), cmd);
    sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    // 按 Claude 起、把参数一行一个写进文件的假 agent
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

    // 开着(默认):Claude 拿到的是 aca 数据目录里的副本,那一行没了;插件目录里的文件没动
    let a = sb.aca(&["new", "argc", &sb.proj(), "-t", "开着起的"]);
    let copy = sb.home.join("data/claude-plugins/t-auto/local");
    assert_eq!(plugin_dirs(&a), [copy.canonicalize().unwrap()]);
    assert_eq!(std::fs::read_to_string(copy.join("commands/go.md")).unwrap(), "---\ndescription: go\n---\ngo $ARGUMENTS\n");
    assert!(copy.join(".claude-plugin/plugin.json").is_file());
    assert_eq!(std::fs::read_to_string(dir.join("claude/commands/go.md")).unwrap(), cmd);
    assert!(!sb.aca(&["plugin", "ls"]).contains("重启"));
    // 关掉:新起的直接拿插件目录;开着起的那个要重启才变
    sb.aca(&["plugin", "setting", "t.auto", "auto", "off"]);
    let b = sb.aca(&["new", "argc", &sb.proj(), "-t", "关着起的"]);
    assert_eq!(plugin_dirs(&b), [dir.join("claude").canonicalize().unwrap()]);
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("「开着起的」") && !ls.contains("「关着起的」") && ls.contains("重启"), "{ls}");
    // 回到默认(开):反过来
    sb.aca(&["plugin", "setting", "t.auto", "auto", "--reset"]);
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(!ls.contains("「开着起的」") && ls.contains("「关着起的」"), "{ls}");
}

#[test]
fn the_delegate_plugin_says_continue_to_a_worker_whose_turn_failed_for_a_transient_reason() {
    let Some(sb) = Sandbox::new("delegate") else { return };
    sb.screen_tools();
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/delegate").canonicalize().unwrap();
    assert!(sb.aca(&["plugin", "link", &plugin.to_string_lossy()]).contains("链接了 aca.delegate"));
    // 它给 Claude、Codex 带同一个插件目录,里面有 /delegate。文件写着模型自己不调它,设置 auto(默认开)开着时不作数
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("● aca.delegate") && ls.contains("带给 claude、codex"), "{ls}");
    let cmd = std::fs::read_to_string(plugin.join("claude/commands/delegate.md")).unwrap();
    assert!(cmd.starts_with("---\n") && cmd.contains("\ndisable-model-invocation: true\n"));
    assert_eq!(sb.aca(&["plugin", "setting", "aca.delegate", "auto"]), "true");
    // 设置:默认值、改了记进 plugins.toml、读得出来;替它说的那句改成「继续」
    assert_eq!(sb.aca(&["plugin", "setting", "aca.delegate", "worker"]), "codex");
    sb.aca(&["plugin", "setting", "aca.delegate", "say", "继续"]);
    assert_eq!(sb.aca(&["plugin", "setting", "aca.delegate", "say"]), "继续");
    assert!(sb.aca(&["plugin", "setting", "aca.delegate"]).contains("say = 继续(改过)"));
    assert!(!sb.run(&["plugin", "setting", "aca.delegate", "nope", "1"]).status.success());
    // 照默认值的类型读:整数的填字不行;字的写 true 也还是字
    let o = sb.run(&["plugin", "setting", "aca.delegate", "max_continue", "many"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("要是整数"));
    sb.aca(&["plugin", "setting", "aca.delegate", "say", "true"]);
    assert!(std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("say = \"true\""));
    sb.aca(&["plugin", "setting", "aca.delegate", "say", "继续"]);

    let lead = sb.aca(&["new", "fake", &sb.proj()]);
    let w = sb.aca(&["new", "scr", &sb.proj(), "--parent", &lead]);
    let mine = sb.aca(&["new", "scr", &sb.proj(), "--parent", "-"]);
    // 钩子是 hook 进程起的,继承它的环境:测试里不等 30 秒
    let fail = |id: &str, error: &str| {
        let pane = sb.pane(id);
        sb.set_screen(id, &pane, "❯ ");
        // 等假 agent 把这一屏画出来:不然它清屏重画时会把替它打进去的「继续」一起擦掉
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
    // 派出去的、临时的错误:替它说「继续」
    let pane = fail(&w, "API Error: 529 Overloaded");
    let log = || std::fs::read_to_string(sb.home.join("state/plugin-logs/aca.delegate.log")).unwrap_or_default();
    assert!(sb.wait_screen(&pane, "继续").contains("继续"), "{}\n{}", sb.tmux(&["capture-pane", "-p", "-t", &pane]), log());
    let log = log();
    assert!(log.contains("第 1 次替它说「继续」"), "{log}");
    // 再试也一样的、你自己开的(没有 parent):不碰
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
    // 它自己重试几次也不行,这一轮带着错误结束:不跑任何 hook,只在记录里留一条,屏幕上停了
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 1;
    let failed = format!(
        r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"t1","completed_at":{now},"error":{{"message":"stream disconnected before completion: Our servers are currently overloaded.","codex_error_info":{{"response_stream_disconnected":{{"http_status_code":null}}}}}}}}}}"#
    );
    std::fs::write(&rollout, format!("{started}\n{failed}\n")).unwrap();
    sb.set_screen(&w, &pane, "› ");
    // 在等它的那个看出来了:出错、哪一类、原文(不是拿上一轮的回复冒充跑完)
    let o = child.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).to_string();
    assert_eq!(o.status.code(), Some(3), "{said}");
    assert!(said.contains("出错(临时的") && said.contains("Our servers are currently overloaded"), "{said}");
    let is_failed = |l: &&str| l.contains(r#""event":"turn.failed""#);
    let ev = sb.aca(&["events", "-n", "100"]).lines().find(is_failed).unwrap().to_string();
    assert!(ev.contains("\"kind\":\"transient\""), "{ev}");
    // 派它的那个要了 failed 通知:收到了,说了是哪一类
    let notice = sb.json(&["inbox", &lead, "--json"]);
    assert_eq!(notice[0]["what"], "turn.failed");
    assert!(notice[0]["text"].as_str().unwrap().contains("临时的"), "{notice}");
    // 只补一次:再看一眼不会再发
    sb.aca(&["ls"]);
    let all = sb.aca(&["events", "-n", "100"]);
    assert_eq!(all.lines().filter(is_failed).count(), 1, "{all}");
    // 接着说一句(它开了新的一轮,记录里有了开始):错误清掉,又在跑
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
    // 这个账号只信任了 SessionStart、UserPromptSubmit 两条 hook,Stop 没信任:每一轮只报开始
    hook(&serde_json::json!({"hook_event_name": "SessionStart", "session_id": "th-1", "transcript_path": rollout}).to_string());
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"改登录页"}"#);
    sb.set_screen(&w, &pane, "• Working (5s • esc to interrupt)");
    assert_eq!(show()["state"], "running");
    // 屏幕上停了、记录里还没写结束(刚停下的那一下,或者按 Esc 打断的):不当成跑完
    sb.set_screen(&w, &pane, "› ");
    assert!(sb.until(|| show()["state"] == "idle"), "{}", show());
    // 跑完了:Codex 不跑没信任的 Stop hook,也不提示,只在记录里留一条
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let done = format!(
        r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"t1","completed_at":{at},"last_agent_message":"登录页改好了","error":null}}}}"#
    );
    std::fs::write(&rollout, format!("{started}\n{done}\n")).unwrap();
    // 有人看一眼就认出来了:跑完没看,最后一条回复是记录里那条,跑完的时刻是它真跑完的时刻(不是看的这一刻)
    let v = show();
    assert_eq!(
        (v["state"].as_str(), v["run"]["status"].as_str(), v["run"]["done"].as_u64()),
        (Some("unread"), Some("done"), Some(at)),
        "{v}"
    );
    assert_eq!(sb.aca(&["output", &w]), "登录页改好了");
    // 派它的那个要了 done 通知:收到了
    let notice = sb.json(&["inbox", &lead, "--json"]);
    assert_eq!(notice[0]["what"], "turn.done", "{notice}");
    // 只补一次:再看几眼不会再发
    sb.aca(&["ls"]);
    sb.aca(&["ls"]);
    let is_done = |l: &&str| l.contains(r#""event":"turn.done""#);
    let all = sb.aca(&["events", "-n", "100"]);
    assert_eq!(all.lines().filter(is_done).count(), 1, "{all}");
    // 看过了,再说一句:新的一轮刚开始、记录里还没写上时,上一轮那条结束不算这一轮的
    sb.aca(&["seen", &w]);
    assert_eq!(show()["state"], "idle");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"th-1","prompt":"再改注册页"}"#);
    let v = show();
    assert_eq!((v["state"].as_str(), v["run"]["status"].as_str()), (Some("idle"), Some("running")), "{v}");
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

    // 在仓库的子目录里建:worktree 开在仓库的 .worktrees/ 下,会话起在里面同一个子目录,分支就叫给的名字
    let o = sb.run(&["new", "fake", &repo.join("sub").to_string_lossy(), "-w", "feat/x"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let wt = repo.join(".worktrees/feat-x");
    assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("worktree {}(分支 feat/x)", wt.display())));
    assert_eq!(sb.json(&["show", &id, "--json"])["dir"], wt.join("sub").to_string_lossy().as_ref());
    assert_eq!((git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), git(&wt, &["rev-parse", "HEAD"])), ("feat/x".to_string(), head));
    // 主仓库的 git status 里看不到它
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    // 同一个分支再建一个:接着用那个 worktree
    let id2 = sb.aca(&["new", "fake", &repo.to_string_lossy(), "--worktree", "feat/x"]);
    assert_eq!(sb.json(&["show", &id2, "--json"])["dir"], wt.to_string_lossy().as_ref());

    // 还有别的会话在里面:删一个,worktree 留着
    assert!(sb.aca(&["rm", &id]).contains("还有别的会话在用"));
    // 有没跟踪的文件(探针脚本之类):留着,不加 --force
    std::fs::write(wt.join("probe.sh"), "echo\n").unwrap();
    assert!(sb.aca(&["rm", &id2]).contains("留着"));
    assert!(wt.is_dir());
    // 干净了:删 worktree,分支和它上面的提交都还在
    std::fs::remove_file(wt.join("probe.sh")).unwrap();
    std::fs::write(wt.join("b.txt"), "b\n").unwrap();
    git(&wt, &["add", "b.txt"]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    let id3 = sb.aca(&["new", "fake", &repo.to_string_lossy(), "-w", "feat/x"]);
    let said = sb.aca(&["rm", &id3]);
    assert!(said.contains("删了,分支 feat/x 还在"), "{said}");
    assert!(!wt.exists());
    assert_eq!(git(&repo, &["log", "-1", "--format=%s", "feat/x"]), "work");

    // 不是仓库、分支名不对:不建
    let o = sb.run(&["new", "fake", &sb.home.join("config").to_string_lossy(), "-w", "x"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("不在 git 仓库里"));
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
    // 没在跑:不按
    sb.set_screen(&w, &pane, "❯ ");
    assert!(err(&sb.as_session(&lead, &["interrupt", &w], "")).contains("没在跑"));
    // 在跑:别的会话按不了,派它的那个能
    sb.set_screen(&w, &pane, "✻ Pondering… (3s · ↓ 1k tokens)");
    sb.hook(&w, &pane, r#"{"hook_event_name":"UserPromptSubmit","session_id":"c1","prompt":"写一篇长文"}"#);
    assert!(err(&sb.as_session(&other, &["interrupt", &w], "")).contains("不是你派出去的"));
    let o = sb.as_session(&lead, &["interrupt", &w], "");
    assert!(o.status.success(), "{}", err(&o));
    // 按下的是 Esc(假 agent 不读键,终端回显成 ^[)
    assert!(sb.wait_screen(&pane, "^[").contains("^["));
    // Claude 停下不报 hook:aca 自己记成空着,事件里写着谁按的
    assert_eq!(sb.json(&["show", &w, "--json"])["run"]["status"], "idle");
    let ev = sb.aca(&["events", "-n", "100"]).lines().find(|l| l.contains("turn.interrupted")).unwrap().to_string();
    assert!(ev.contains(&format!("\"by\":\"{lead}\"")), "{ev}");
}

#[test]
fn plugins_toml_says_which_plugins_with_what_settings_and_sync_makes_it_so() {
    let Some(sb) = Sandbox::new("decl") else { return };
    sb.screen_tools();
    // 一个本地插件:一项设置、一个带参数的能力、一个只对 fake 会话起的事件钩子
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
    // 链接:plugins.toml 里记下它从哪来
    sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    let text = std::fs::read_to_string(&decl).unwrap();
    assert!(text.contains("[plugins.\"t.decl\"]") && text.contains(&dir.canonicalize().unwrap().to_string_lossy().into_owned()), "{text}");
    // 能力的参数照清单的类型变成 JSON,设置在环境里
    assert_eq!(sb.aca(&["plugin", "run", "t.decl.say", "who=me", "n=2"]), r#"hi {"n":2,"who":"me"}"#);
    let o = sb.run(&["plugin", "run", "t.decl.say", "n=2"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("who"));
    // plugins.toml 里改设置:下一次就是新的
    std::fs::write(&decl, text.trim_end().to_string() + "\nsettings = { greeting = \"yo\" }\n").unwrap();
    assert!(sb.aca(&["plugin", "run", "t.decl.say", "who=me"]).starts_with("yo "));
    // 事件钩子的 if:fake 起的会话跑,scr 起的不跑
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
    // 命令行停它:plugins.toml 里记下停着,别的行、设置原样
    sb.aca(&["plugin", "disable", "t.decl"]);
    let text = std::fs::read_to_string(&decl).unwrap();
    assert!(text.contains("enabled = false") && text.contains("greeting = \"yo\""), "{text}");
    // 从 plugins.toml 里划掉:sync 后本机也没了(链接的目录不动)
    let kept = text.lines().take_while(|l| !l.contains("t.decl")).collect::<Vec<_>>().join("\n");
    std::fs::write(&decl, kept + "\n").unwrap();
    assert!(sb.aca(&["plugin", "sync"]).contains("不再用 t.decl"));
    assert!(sb.aca(&["plugin", "ls"]).is_empty() && dir.join("aca-plugin.toml").is_file());
    // 写回去:sync 照它链接,停着的照样停着
    std::fs::write(&decl, format!("[plugins.\"t.decl\"]\nsource = \"{}\"\nenabled = false\n", dir.display())).unwrap();
    assert!(sb.aca(&["plugin", "sync"]).contains("链接了 t.decl"));
    assert!(sb.aca(&["plugin", "ls"]).starts_with("○ t.decl"));
    assert!(sb.aca(&["plugin", "sync"]).contains("没什么要做的"));
}

#[test]
fn a_plugin_mounts_its_directory_and_mcp_on_claude_and_codex_and_its_commands_become_tools() {
    let Some(sb) = Sandbox::new("mount") else { return };
    // 插件:给两家的目录(一个斜杠命令)、一个 MCP 服务、一个露给 agent 的能力
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
    // 两个假 agent:按 Claude / Codex 起,把参数一行一个写进文件;Codex 那个账号有自己的配置目录
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

    // Claude:插件目录 --plugin-dir,两个 MCP 服务(插件自己的、能力包成的 aca)写在一份 --mcp-config 里
    let c = sb.aca(&["new", "argc", &sb.proj()]);
    let args = args_of(&c);
    assert!(after(&args, "--plugin-dir").contains(&dir.join("agent").canonicalize().unwrap().to_string_lossy().into_owned()), "{args:?}");
    let cfg_file = after(&args, "--mcp-config");
    assert_eq!(cfg_file.len(), 1, "{args:?}");
    let mcp: Value = serde_json::from_str(&std::fs::read_to_string(&cfg_file[0]).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["aca-t-both"]["args"], serde_json::json!(["plugin", "mcp", "t.both"]));
    assert_eq!(mcp["mcpServers"]["aca"]["args"], serde_json::json!(["plugin", "tools"]));

    // Codex:插件 MCP 和能力的 MCP 走 -c mcp_servers(会话 id 写在 env 里),插件目录变成它认得的副本、按会话打开
    let x = sb.aca(&["new", "argx:t", &sb.proj()]);
    let flags = after(&args_of(&x), "-c");
    let server = flags.iter().find(|f| f.starts_with("mcp_servers.aca-t-both=")).unwrap_or_else(|| panic!("{flags:?}"));
    assert!(server.contains("\"plugin\"") && server.contains(&format!("ACA_SESSION_ID = \"{x}\"")), "{server}");
    assert!(
        flags.iter().any(|f| f.starts_with("mcp_servers.aca=")) && flags.contains(&"plugins.t-both@aca.enabled=true".to_string()),
        "{flags:?}"
    );
    let link = codex_home.join("plugins/cache/aca");
    assert_eq!(std::fs::read_link(&link).unwrap(), sb.home.join("data/codex-plugins"));
    let copy = sb.home.join("data/codex-plugins/t-both/local");
    let skill = std::fs::read_to_string(copy.join("skills/greet/SKILL.md")).unwrap();
    assert!(skill.starts_with("---\nname: greet\ndescription: 打招呼\n---\n"), "{skill}");
    assert!(std::fs::read_to_string(copy.join(".codex-plugin/plugin.json")).unwrap().contains("\"t-both\""));

    // 停了插件:新起的什么都不多带
    sb.aca(&["plugin", "disable", "t.both"]);
    let bare = sb.aca(&["new", "argc", &sb.proj()]);
    let args = args_of(&bare);
    assert!(!args.iter().any(|a| a == "--mcp-config" || a == "--plugin-dir"), "{args:?}");
    sb.aca(&["plugin", "enable", "t.both"]);

    // 能力包成的 MCP 服务:握手、列出工具、调一个(会话是 agent 环境里那个)
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

    // 插件自己的 MCP 服务:切到插件目录、带上插件的环境,换成它的命令
    let o = sb.cmd().args(["plugin", "mcp", "t.both"]).env("ACA_SESSION_ID", &x).output().unwrap();
    let said = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert_eq!(said, format!("mcp of t.both for {x} in {}", dir.canonicalize().unwrap().display()));
}

/// 经转接和 gateway 说 MCP 的一个连接(测试用):一直开着,一问一答
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

    /// 问一句,等编号对得上的回答(中间来的通知跳过)
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
    // gateway 这个插件(仓库里的),和两个插件:各有一个露给 agent 的能力,MCP 都是 aca 自己的能力服务(一个真的 MCP 服务);
    // 一个共用,一个每个会话一份
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
    // 测试里起的 gateway 一分钟没人连就自己退
    let decl = sb.home.join("config/plugins.toml");
    let text = std::fs::read_to_string(&decl)
        .unwrap()
        .replace("[plugins.\"aca.gateway\"]", "[plugins.\"aca.gateway\"]\nsettings = { idle_minutes = 1 }");
    std::fs::write(&decl, text).unwrap();
    // 按 Claude 起的假 agent,把参数写进文件
    write(
        &sb.home.join("args.sh"),
        &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}/args-'\"$ACA_SESSION_ID\"\nexec sleep 600\n", sb.home.display()),
    );
    let cfg = sb.home.join("config/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap()
        + &format!("[tools.argc]\nkind = \"claude\"\ncmd = [\"/bin/sh\", \"{}\"]\n", sb.home.join("args.sh").display());
    std::fs::write(&cfg, text).unwrap();

    // gateway 开着:agent 只挂一个 aca,是到 gateway 的转接;插件自己的 MCP 不再直接挂
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

    // 两个会话同时连着:工具一样;调用时插件命令拿到的是调它的那个会话
    let mut ca = GwConn::open(&sb, &a);
    let mut cb = GwConn::open(&sb, &b);
    let names = ca.tools();
    for want in ["t-gw__who", "t-ses__who", "t-gw__t-gw__who", "t-ses__t-ses__who"] {
        assert!(names.iter().any(|n| n == want), "{want}:{names:?}");
    }
    assert_eq!(cb.tools().len(), names.len());
    assert_eq!(ca.call(10, "t-gw__who"), a);
    assert_eq!(cb.call(11, "t-gw__who"), b);
    // 共用的那个插件 MCP:只有一份,两个会话的调用都经它,它从 _meta 里知道是谁
    assert_eq!(ca.call(12, "t-gw__t-gw__who"), a);
    assert_eq!(cb.call(13, "t-gw__t-gw__who"), b);
    // 每会话一份的:各有各的进程,环境里就是自己的会话
    assert_eq!(cb.call(14, "t-ses__t-ses__who"), b);
    let st = sb.aca(&["plugin", "gateway", "status"]);
    assert!(st.contains("2 个连接"), "{st}");
    assert_eq!(st.matches("服务 t-gw:ready").count(), 1, "{st}");
    assert!(st.contains(&format!("服务 t-ses(会话 {a}):ready")) && st.contains(&format!("服务 t-ses(会话 {b}):ready")), "{st}");
    assert!(st.contains("✓ t-gw t-gw__who"), "{st}");
    // 只对 a 关掉每会话一份的那个:a 看不到了,b 照旧;再打开就回来
    assert!(sb.aca(&["plugin", "gateway", "off", "t-ses", "--session", &a]).contains("不再"));
    assert!(!ca.tools().iter().any(|n| n == "t-ses__t-ses__who"));
    assert!(cb.tools().iter().any(|n| n == "t-ses__t-ses__who"));
    sb.aca(&["plugin", "gateway", "on", "t-ses", "--session", &a]);
    assert!(ca.tools().iter().any(|n| n == "t-ses__t-ses__who"));
    // [mcps.*] 里没有的收不进来
    let o = sb.run(&["plugin", "gateway", "add", "nope"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("[mcps.nope]"));
    // 一个会话断了:它那份每会话的收掉,共用的留着
    cb.close();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let st = sb.aca(&["plugin", "gateway", "status"]);
    assert!(st.contains("1 个连接") && !st.contains(&format!("会话 {b}):ready")) && st.contains("服务 t-gw:ready"), "{st}");
    ca.close();
    // 停掉:不留进程
    assert!(sb.aca(&["plugin", "gateway", "stop"]).contains("停了"));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(sb.aca(&["plugin", "gateway", "status"]).contains("没在跑"));
}

#[test]
fn the_core_plugin_lets_an_agent_dispatch_a_worker_message_it_and_only_manage_its_own() {
    let Some(sb) = Sandbox::new("core") else { return };
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/core").canonicalize().unwrap();
    sb.aca(&["plugin", "link", &core.to_string_lossy()]);
    let lead = sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    let mine = sb.aca(&["new", "fake", &sb.proj(), "--parent", "-"]);
    // 主线的 agent 经能力服务调 aca.core 的工具(它环境里的会话就是主线)
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
    assert_eq!(tools.len(), 6, "{tools:?}");
    assert!(tools.contains(&"aca-core__dispatch".to_string()) && tools.contains(&"aca-core__interrupt".to_string()));
    let mut call = |id: u64, name: &str, args: Value| -> (bool, String) {
        let r = ask(id, "tools/call", serde_json::json!({ "name": name, "arguments": args }));
        (r["result"]["isError"].as_bool() == Some(true), r["result"]["content"][0]["text"].as_str().unwrap_or("").trim().to_string())
    };
    // 派一个 worker:挂在主线下面,话也发进去了
    let (err, w) = call(3, "aca-core__dispatch", serde_json::json!({ "tool": "fake", "task": "看看登录页", "title": "读登录" }));
    assert!(!err, "{w}");
    let w = w.lines().last().unwrap().to_string();
    let (_, shown) = call(4, "aca-core__sessions", serde_json::json!({ "id": w }));
    let v: Value = serde_json::from_str(&shown).unwrap();
    assert_eq!((v["parent"].as_str(), v["title"].as_str()), (Some(lead.as_str()), Some("读登录")));
    // 递话:进了它的收件箱
    let (err, said) = call(5, "aca-core__message", serde_json::json!({ "to": w, "text": "顺便看一下注册页" }));
    assert!(!err, "{said}");
    assert!(sb.aca(&["inbox", &w]).contains("顺便看一下注册页"));
    // 不是它派出去的:中止不了
    let (err, said) = call(6, "aca-core__interrupt", serde_json::json!({ "id": mine }));
    assert!(err, "{said}");
    // 列会话
    let (_, all) = call(7, "aca-core__sessions", serde_json::json!({}));
    assert!(all.contains(&lead) && all.contains(&w), "{all}");
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn first_party_plugins_install_by_name_bring_what_they_need_and_running_sessions_hear_they_need_a_restart() {
    let Some(sb) = Sandbox::new("bundled") else { return };
    sb.aca(&["new", "fake", &sb.proj(), "-t", "主线"]);
    // 没有插件时起的:不用重启
    assert!(!sb.aca(&["plugin", "ls"]).contains("主线"));
    // 自带的不写路径;装完说带了什么
    let out = sb.aca(&["plugin", "install", "aca.delegate"]);
    assert!(out.contains("装好了 aca.delegate") && out.contains("给 agent:claude、codex 的插件目录"), "{out}");
    let copy = sb.home.join("data/first-party/delegate");
    assert!(copy.join("aca-plugin.toml").exists() && copy.join("claude/commands/delegate.md").exists());
    let decl = std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap();
    assert!(decl.contains("[plugins.\"aca.delegate\"]") && !decl.contains("source"), "{decl}");
    // 它给 Claude 带东西了:开着的「主线」起来时没有,要重启
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("aca.delegate") && ls.contains("aca 自带") && ls.contains("「主线」") && ls.contains("重启"), "{ls}");
    // 写了 requires 的插件:要的第一方插件一起装上
    let mine = sb.home.join("needs-core");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("aca-plugin.toml"), "id = \"my.needs\"\nname = \"N\"\nversion = \"1\"\nrequires = [\"aca.core\"]\n").unwrap();
    let out = sb.aca(&["plugin", "install", &mine.to_string_lossy()]);
    assert!(out.contains("aca.core") && sb.aca(&["plugin", "ls"]).contains("aca.core"), "{out}");
    // plugins.toml 里只写个表头,sync 也装得上自带的
    let mut decl = std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap();
    decl.push_str("\n[plugins.\"aca.remote\"]\n");
    std::fs::write(sb.home.join("config/plugins.toml"), decl).unwrap();
    assert!(sb.aca(&["plugin", "sync"]).contains("装了 aca.remote"));
    // 视图:命令行打出它的输出
    assert!(sb.aca(&["plugin", "view", "aca.remote.tasks"]).contains("没有远端任务"));
    assert!(sb.aca(&["plugin", "actions"]).contains("aca.remote.tasks  视图  远端任务"));
    // 删:aca 写出来的那份一起删
    sb.aca(&["plugin", "remove", "aca.remote"]);
    assert!(!sb.home.join("data/first-party/remote").exists());
    // 自带的再装一遍:就地重写、重新登记,不报已经有了
    let o = sb.run(&["plugin", "install", "aca.delegate"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn mcps_scattered_in_accounts_are_listed_and_taken_into_the_gateway_without_touching_the_accounts() {
    let Some(sb) = Sandbox::new("import") else { return };
    let gwp = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/gateway").canonicalize().unwrap();
    sb.aca(&["plugin", "link", &gwp.to_string_lossy()]);
    // 一个假的家目录:默认的 Claude、登记了的 claude:sub、默认的 Codex 各有几个 MCP
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
    // 只列:同名同写法的并成一个,HTTP 的标出来;key 的值不打出来
    let out = run(&["plugin", "gateway", "import"]);
    assert!(out.contains("github  [claude claude:sub]  npx -y gh  env TOKEN") && !out.contains("s3cret"), "{out}");
    assert!(
        out.contains("web  [claude]") && out.contains("HTTP") && out.contains("fs  [codex]  fs-mcp") && out.contains("--apply"),
        "{out}"
    );
    assert!(!std::fs::read_to_string(&cfg).unwrap().contains("[mcps."));
    // 收进来:写进 aca 的 config.toml、gateway 的 servers;账号里的原样
    let out = run(&["plugin", "gateway", "import", "--apply"]);
    assert!(out.contains("收进了 gateway:fs github") && out.contains("账号里的那份还在"), "{out}");
    let conf = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        conf.contains("[mcps.github]") && conf.contains("TOKEN = \"s3cret\"") && conf.contains("[mcps.fs]") && !conf.contains("[mcps.web]"),
        "{conf}"
    );
    assert!(std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("servers = [\"fs\", \"github\"]"));
    assert_eq!(std::fs::read_to_string(home.join(".claude.json")).unwrap(), before);
    // 再来一遍:已经有的不重复写
    run(&["plugin", "gateway", "import", "--apply"]);
    assert_eq!(std::fs::read_to_string(&cfg).unwrap().matches("[mcps.github]").count(), 1);
    // drop 移出 gateway
    assert!(run(&["plugin", "gateway", "drop", "fs"]).contains("不再经 gateway"));
    assert!(std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("servers = [\"github\"]"));
}

/// 列表视图:aca plugin view 打成一行一条,--json 原样;它的键要跑的能力没露给菜单也能从视图跑
#[test]
fn a_list_view_prints_as_rows_and_its_keys_commands_run_though_not_in_the_menu() {
    let Some(sb) = Sandbox::new("listview") else { return };
    sb.aca(&["plugin", "install", "aca.remote"]);
    let text = sb.aca(&["plugin", "view", "aca.remote.tasks"]);
    assert!(text.contains("还没配 server") && text.contains("没有远端任务"), "{text}");
    let raw: Value = serde_json::from_str(&sb.aca(&["plugin", "view", "--json", "aca.remote.tasks"])).unwrap();
    assert_eq!(raw["rows"], serde_json::json!([]));
    // collect 只露给 cli,菜单里没有;键 r 要跑它
    let actions = sb.aca(&["plugin", "actions"]);
    assert!(actions.contains("aca.remote.tasks"), "{actions}");
    // --json:一个能力一行(全局的 --json 在分到 plugin 之前就拿走了,照样认)
    let ids: Vec<String> = sb
        .aca(&["plugin", "actions", "--json"])
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["aca.remote.run", "aca.remote.new", "aca.remote.list", "aca.remote.rm", "aca.remote.collect", "aca.remote.check"]);
    assert!(sb.aca(&["plugin", "run", "aca.remote.collect"]).contains("取过了,没有新的"));
    // aca mounts、aca remote 挪进了插件:指路
    let o = sb.run(&["mounts", "nope"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("挪进了插件 aca.tailnet"));
    let o = sb.run(&["remote", "run", "gpu1", "--", "make"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("挪进了插件 aca.remote"));
}

/// aca.remote 整个在插件里(remote.sh,sh + jq + curl + ssh):用假的 server(curl)和假的远端(ssh 在本机跑)走一遍 ——
/// 派出去、远端跑完回调、到点去取、结果放进派它的会话的收件箱、定时开了又关;失联、会话没了、不在会话里登记的、
/// 连不上 server、撤掉
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
    // PATH:假的在前(真的 curl、ssh 在 /usr/bin);jq、tmux 单独接进来
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

    // 还没配 server:说去哪配
    let (good, out) = run(&["plugin", "run", "aca.remote.run", "--session", &lead, "host=gpu1", "command=true"]);
    assert!(!good && out.contains("还没配 server") && out.contains("aca plugin setting aca.remote url"), "{out}");
    sb.aca(&["plugin", "setting", "aca.remote", "url", "https://hook.test/"]);
    sb.aca(&["plugin", "setting", "aca.remote", "token", "right-token-0123456789"]);
    // 口令打码列;记它的 plugins.toml 只有自己能读
    assert!(!sb.aca(&["plugin", "setting", "aca.remote"]).contains("right-token"));
    let decl = std::fs::metadata(sb.home.join("config/plugins.toml")).unwrap();
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&decl.permissions()) & 0o777, 0o600);
    write(&fake.join("token"), "wrong-token-0123456789");
    let (good, out) = run(&["plugin", "run", "aca.remote.check"]);
    assert!(!good && out.contains("口令不对"), "{out}");
    write(&fake.join("token"), "right-token-0123456789");
    let out = ok(&["plugin", "run", "aca.remote.check"]);
    assert!(out.contains("连上了,口令也对 · jobhook 0.1.0"), "{out}");

    // 连不上那台机器:说为什么,刚登记的撤掉(server、本机都没留),定时没开着
    let (good, out) = run(&["plugin", "run", "aca.remote.run", "--session", &lead, "host=nohost", "command=true"]);
    assert!(!good && out.contains("没在 nohost 上跑起来:ssh: Could not resolve hostname nohost"), "{out}");
    assert_eq!(std::fs::read_dir(&hub).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&tasks).map(|d| d.count()).unwrap_or(0), 0);
    assert_eq!(timer()["on"], false);

    // 派出去:远端后台跑起来,马上返回;定时开了
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
    assert!(out.contains("已在 gpu1 后台跑起来") && out.contains("日志:gpu1:~/.aca-hooks/") && !out.contains("不在 aca 的会话里"), "{out}");
    let id =
        std::fs::read_dir(&tasks).unwrap().flatten().find_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(String::from)).unwrap();
    assert_eq!(id.len(), 32);
    let t = task(&id);
    assert_eq!((t["session"].as_str(), t["title"].as_str(), t["within"].as_i64()), (Some(lead.as_str()), Some("lead"), Some(21600)), "{t}");
    let tm = timer();
    assert_eq!((tm["on"].as_bool(), tm["scheduled"].as_bool()), (Some(true), Some(true)), "{tm}");
    assert!(ok(&["plugin", "run", "aca.remote.list"]).contains("gpu1 上的「训练」  在等 · 已经"));
    // 远端跑完回调到 server(假的远端在本机后台跑,等它)
    let rec = hub.join(format!("{id}.json"));
    assert!(sb.until(|| std::fs::read_to_string(&rec).is_ok_and(|t| t.contains("\"done\""))), "没回调");
    assert!(std::fs::read_to_string(fake.join(format!("remote/.aca-hooks/{id}.log"))).unwrap().contains("第一行"));

    // 到点去取:结果放进 lead 的收件箱(发件人是你),server 上的取走了;都了结了,定时在这一次跑完后关掉
    assert_eq!(ok(&["plugin", "timer", "tick", "aca.remote.collect"]), "");
    let got = inbox(&lead);
    assert!(got.contains("[aca 远端任务] gpu1 上的「训练」结束了:失败了,退出码 3") && got.contains("最后一行"), "{got}");
    assert!(got.contains("\"from\":\"you\""), "{got}");
    assert!(!rec.exists());
    assert_eq!(task(&id)["state"], "sent");
    let tm = timer();
    assert_eq!((tm["on"].as_bool(), tm["scheduled"].as_bool(), tm["rc"].as_i64()), (Some(false), Some(false), Some(0)), "{tm}");
    assert!(ok(&["plugin", "log", "aca.remote"]).contains("gpu1 上的「训练」:发给会话了"));
    // 列表:✕ 退出码,⏎ 复制看日志的命令
    let v: Value = serde_json::from_str(&ok(&["plugin", "view", "--json", "aca.remote.tasks"])).unwrap();
    let row = &v["rows"][0];
    assert_eq!((row["mark"].as_str(), row["tone"].as_str()), (Some("✕"), Some("bad")), "{v}");
    assert!(row["right"].as_str().unwrap().starts_with("退出码 3 · ") && row["right"].as_str().unwrap().ends_with("发给会话了"), "{v}");
    assert_eq!(row["fields"]["log_cmd"], format!("ssh gpu1 tail -n 50 ~/.aca-hooks/{id}.log"));
    assert!(row["detail"][0].as_str().unwrap().contains("会话「lead」"), "{v}");

    // 只登记的(自己接回调):失联告诉会话一次;之后回调到了照样发
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
    assert!(ok(&["plugin", "run", "aca.remote.list"]).contains("失联 · 过了 1d 没回调"));
    set_hub(&lost, &|v| {
        v["state"] = "done".into();
        v["rc"] = 0.into();
        v["tail"] = "ok".into();
    });
    ok(&["plugin", "run", "aca.remote.collect"]);
    assert!(inbox(&other).contains("「手动」结束了:退出码 0"));

    // 会话没了、不在会话里登记的:结果留在列表里,说为什么
    let gone = new_session("gone");
    let id_gone = ok(&["plugin", "run", "aca.remote.new", "--session", &gone, "note=没人收"]).lines().next().unwrap()[3..].to_string();
    sb.aca(&["rm", &gone]);
    let out = ok(&["plugin", "run", "aca.remote.new", "note=外面"]);
    assert!(out.contains("不在 aca 的会话里"), "{out}");
    let id_out = out.lines().next().unwrap()[3..].to_string();
    for id in [&id_gone, &id_out] {
        set_hub(id, &|v| {
            v["state"] = "done".into();
            v["rc"] = 1.into();
        });
    }
    let out = ok(&["plugin", "run", "aca.remote.collect"]);
    assert!(out.contains("「没人收」:结束了,没送达(会话已经不在了)") && out.contains("「外面」:结束了,没送达(登记时不在会话里)"), "{out}");
    assert_eq!(task(&id_gone)["state"], "kept");
    assert_eq!(timer()["on"], false);

    // 连不上 server:这次没取到,不算失败;撤掉一个在等的:server 上也删
    let waiting = ok(&["plugin", "run", "aca.remote.new", "--session", &lead, "note=等着"]).lines().next().unwrap()[3..].to_string();
    write(&fake.join("down"), "");
    let out = ok(&["plugin", "run", "aca.remote.collect"]);
    assert!(out.contains("这次没取到:连不上 server:(7) Failed to connect"), "{out}");
    let (good, out) = run(&["plugin", "run", "aca.remote.rm", &format!("id={}", &waiting[..6])]);
    assert!(!good && out.contains("连不上 server"), "{out}");
    std::fs::remove_file(fake.join("down")).unwrap();
    // 另一个正在取:这次不取;锁放了很久的当没人要
    write(&tasks.join(".lock"), &format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()));
    assert!(ok(&["plugin", "run", "aca.remote.collect"]).contains("另一个正在取"));
    write(&tasks.join(".lock"), "1000");
    assert!(!ok(&["plugin", "run", "aca.remote.collect"]).contains("另一个正在取"));
    assert!(ok(&["plugin", "run", "aca.remote.rm", &format!("id={}", &waiting[..6])]).contains("已撤 「等着」"));
    assert!(!hub.join(format!("{waiting}.json")).exists() && !tasks.join(format!("{waiting}.json")).exists());
    let (good, out) = run(&["plugin", "run", "aca.remote.rm", "id=zz"]);
    assert!(!good && out.contains("没有 zz 开头的 id"), "{out}");
    assert_eq!(timer()["on"], false);
}

/// 定时:清单里的 [[timers]],到点 launchd 跑 aca plugin timer tick(测试里不碰 launchd:plist 写在 state/timers 里,到点由这里 tick)。
/// 没写 manual 的跟着插件开关;manual 的插件自己开关,跑着的那一次里关掉自己等跑完才卸;清单改了、插件删了照着收拾
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
    // 跑完一次就关掉自己(像远端任务都了结了)
    write(&dir.join("poll.sh"), "echo polled >> \"$ACA_PLUGIN_STATE_DIR/polls\"\n\"$ACA_BIN\" plugin timer off \"$ACA_PLUGIN_ID.poll\"\n");
    let d = dir.to_str().unwrap();
    let out = sb.aca(&["plugin", "link", d]);
    assert!(!out.contains("提醒"), "{out}");
    let timers = sb.home.join("state/timers");
    let plist = |run: &str| timers.join(format!("com.aca.timer.t-timed.{run}.plist"));
    let count = |f: &str| std::fs::read_to_string(sb.home.join("state/plugins/t.timed").join(f)).unwrap_or_default().lines().count();
    let rows = || -> Vec<Value> { sb.aca(&["plugin", "timer", "--json"]).lines().map(|l| serde_json::from_str(l).unwrap()).collect() };

    // 链接上:跟着插件的那个开了(plist 跑这个 aca 的 tick),manual 的关着
    let ls = sb.aca(&["plugin", "timer"]);
    assert!(ls.contains("● t.timed.tick  每 10m · 开着") && ls.contains("○ t.timed.poll  每 1m · 插件自己开关 · 关着"), "{ls}");
    let p = std::fs::read_to_string(plist("tick")).unwrap();
    assert!(p.contains("<string>tick</string>\n        <string>t.timed.tick</string>") && p.contains("<integer>600</integer>"), "{p}");
    assert!(p.contains(env!("CARGO_BIN_EXE_aca")), "{p}");
    assert!(!plist("poll").exists());
    // 到点:跑一次,输出进插件的日志,记下退出码
    let o = sb.run(&["plugin", "timer", "tick", "t.timed.tick"]);
    assert!(o.status.success() && o.stdout.is_empty(), "{o:?}");
    assert_eq!(count("ticks"), 1);
    let log = sb.aca(&["plugin", "log", "t.timed"]);
    assert!(log.contains(" timer tick\ntick out"), "{log}");
    assert!(sb.aca(&["plugin", "timer"]).contains("上次 0s 前 ✓"));
    write(&sb.home.join("state/plugins/t.timed/fail"), "");
    sb.aca(&["plugin", "timer", "tick", "t.timed.tick"]);
    assert_eq!(rows()[0]["rc"], 3);
    assert!(sb.aca(&["plugin", "log", "t.timed"]).contains("(退出码 3)"));
    assert!(sb.aca(&["plugin", "timer"]).contains("✕ 退出码 3"));
    // 跟着插件的关不了
    let o = sb.run(&["plugin", "timer", "off", "t.timed.tick"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("跟着插件开关"), "{o:?}");

    // manual 的:插件说开才开;跑着的那一次里它关掉自己,跑完才卸(plist 没了、记着关了),上次跑得怎么样照记
    assert!(sb.aca(&["plugin", "timer", "on", "t.timed.poll"]).contains("开了 t.timed.poll:每 1m 跑一次"));
    assert!(plist("poll").exists());
    sb.aca(&["plugin", "timer", "tick", "t.timed.poll"]);
    assert_eq!(count("polls"), 1);
    assert!(!plist("poll").exists());
    let r = rows().into_iter().find(|r| r["id"] == "t.timed.poll").unwrap();
    assert_eq!((r["on"].as_bool(), r["scheduled"].as_bool(), r["rc"].as_i64()), (Some(false), Some(false), Some(0)), "{r}");
    // 关着的到点了(launchd 那头还没卸干净):不跑
    sb.aca(&["plugin", "timer", "tick", "t.timed.poll"]);
    assert_eq!(count("polls"), 1);
    // run:现在跑一次,输出打出来,关着也跑
    std::fs::remove_file(sb.home.join("state/plugins/t.timed/fail")).unwrap();
    assert!(sb.aca(&["plugin", "timer", "run", "t.timed.tick"]).contains("tick out"));
    assert_eq!(count("ticks"), 3);

    // 插件停着:都卸掉,到点了也不跑;开回来照原来的
    sb.aca(&["plugin", "disable", "t.timed"]);
    assert!(!plist("tick").exists());
    assert!(sb.aca(&["plugin", "timer"]).contains("○ t.timed.tick  每 10m · 插件停着"));
    sb.aca(&["plugin", "timer", "tick", "t.timed.tick"]);
    assert_eq!(count("ticks"), 3);
    sb.aca(&["plugin", "enable", "t.timed"]);
    assert!(plist("tick").exists() && !plist("poll").exists());
    // 清单改了:every 变了重装;定时删了收拾掉
    manifest("20m", false);
    sb.aca(&["plugin", "link", d]);
    assert!(std::fs::read_to_string(plist("tick")).unwrap().contains("<integer>1200</integer>"));
    assert!(!timers.join("t.timed.poll.json").exists());
    assert_eq!(rows().len(), 1);
    // 插件删了:它的定时都没了
    sb.aca(&["plugin", "unlink", "t.timed"]);
    assert!(!plist("tick").exists() && !timers.join("t.timed.tick.json").exists());
    assert!(sb.aca(&["plugin", "timer"]).contains("没有插件写了定时"));
    // 已经没了的到点了:把自己卸掉,什么都不跑
    assert!(sb.run(&["plugin", "timer", "tick", "t.timed.tick"]).status.success());
    assert_eq!(count("ticks"), 3);
}

/// aca.tailnet 整个在插件里(tailnet.sh,sh + jq):用假的 tailscale / netstat / tpub 跑一遍列、撤、挂、发布。
/// aca 只管把它跑起来、把话说成界面用的那种(ACA_LANG):这里不设 ACA_LANG,config.toml 写中文
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
    // 3000 只听 ::1,22 听所有地址,9000 只有 tailscale 自己留的口子,8420 没人听(连着的不算)
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
    // PATH 只给假的和系统目录:机器上真的 tailscale、tpub(~/.local/bin、/opt/homebrew/bin 里)一个都找不到,
    // 测「撤」「发布」时碰不到真的挂载。jq 单独放一个目录接进来
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
    // 像是刚升级了 aca:写出来的还是老的那份(调已经没了的 aca mounts)。跑它之前先换成新的
    write(&sb.home.join("data/first-party/tailnet/tailnet.sh"), "#!/bin/sh\nexec \"$ACA_BIN\" mounts --json\n");
    reset();

    // 列表视图:一行一条,目标活着没,公网 / 前台标出来;发布的开关在上面
    let (ok, out) = run(true, &["plugin", "view", "--json", "aca.tailnet.mounts"]);
    assert!(ok, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["head"], "mac.tail.ts.net\n发布 关着 · 2 样");
    assert_eq!(v["fields"]["publish_to"], "on");
    let row = |id: &str| v["rows"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap_or_else(|| panic!("没有 {id}:{v}")).clone();
    let r = row("https:443/");
    assert_eq!((r["mark"].as_str(), r["tone"].as_str(), r["right"].as_str()), (Some("✕"), Some("bad"), Some("没在听")));
    assert_eq!(r["text"], ":443 /         → 127.0.0.1:8420");
    assert_eq!((r["fields"]["url"].as_str(), r["fields"]["drop"].as_str()), (Some("https://mac.tail.ts.net/"), Some("https:443/")));
    assert_eq!(row("https:8443/")["right"], "在听 · 公网");
    assert_eq!(row("https:8443/docs")["right"], "文件在 · 公网");
    assert_eq!(row("tcp:2222")["right"], "在听");
    let fg = row("fg:https:9000/");
    assert!(fg["fields"].get("drop").is_none() && fg["right"] == "没在听 · 前台", "{fg}");
    // 给人 / agent 看的那份
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.list"]);
    assert!(ok && out.starts_with("mac.tail.ts.net\n✕ https://mac.tail.ts.net/ "), "{out}");
    assert!(out.contains("● https://mac.tail.ts.net:8443/docs  → ") && out.contains("文件在 · 公网"), "{out}");

    // 撤:带上端口和路径;开到公网的用 funnel 撤;撤完写出怎么加回去
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.drop", "target=https:8443/docs"]);
    assert!(
        ok && out.contains("已撤 https://mac.tail.ts.net:8443/docs")
            && out.contains("要加回去:tailscale funnel --bg --https=8443 --set-path=/docs"),
        "{out}"
    );
    run(false, &["plugin", "run", "aca.tailnet.drop", "target=https://mac.tail.ts.net/"]);
    run(false, &["plugin", "run", "aca.tailnet.drop", "target=tcp:2222"]);
    assert_eq!(did(), "funnel --https=8443 --set-path=/docs off\nserve --https=443 --set-path=/ off\nserve --tcp=2222 off\n");
    // 没有这条、前台的:不去敲命令
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.drop", "target=https:443/"]);
    assert!(!ok && out.contains("没有 https:443/ 这一条挂载"), "{out}");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.drop", "target=fg:https:9000/"]);
    assert!(!ok && out.contains("前台挂的撤不了"), "{out}");
    assert_eq!(did().lines().count(), 3);
    // tailscale 说撤了、其实没撤;撤一条带走了别的:都照实说,带走的写出怎么加回去
    reset();
    write(&fake.join("off_ignored"), "");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.drop", "target=https:443/"]);
    assert!(!ok && out.contains("还挂着") && out.contains("tailscale serve --https=443 --set-path=/ off"), "{out}");
    reset();
    write(&fake.join("off_wipes"), "");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.drop", "target=https:443/"]);
    assert!(!ok && out.contains("跟着没了") && out.contains("tailscale serve --bg --tcp=2222 tcp://127.0.0.1:22"), "{out}");
    assert!(out.contains("http://mac.tail.ts.net:8080/hi") && !out.contains("9000"), "{out}");

    // 挂:tailnet 里同一个端口号;目标没在听的提一句;端口号已经有了的不动
    reset();
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.add", "port=3000"]);
    assert!(ok && out.trim() == "已挂 https://mac.tail.ts.net:3000/", "{out}");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.add", "port=5174"]);
    assert!(ok && out.contains("本机 5174 现在没在听"), "{out}");
    for taken in ["443", "2222", "9000"] {
        let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.add", &format!("port={taken}")]);
        assert!(!ok && out.contains("已经挂着别的"), "{taken}: {out}");
    }
    assert_eq!(did(), "serve --bg --https=3000 http://localhost:3000\nserve --bg --https=5174 http://localhost:5174\n");
    // tailscale 没在跑:列表照样出来,写着为什么;挂不了
    reset();
    write(&fake.join("down"), "");
    let (ok, out) = run(false, &["plugin", "view", "--json", "aca.tailnet.mounts"]);
    assert!(ok && out.contains("is Tailscale running"), "{out}");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.add", "port=3000"]);
    assert!(!ok && out.contains("is Tailscale running"), "{out}");

    // 发布:tpub 说的照实转;说开了其实没开的不算
    reset();
    write(&fake.join("tpub"), "busy");
    let (ok, out) = run(true, &["plugin", "run", "aca.tailnet.publish", "to=on"]);
    assert!(!ok && out.contains("没打开:tailnet 的 :8443 上挂着别的东西"), "{out}");
    write(&fake.join("tpub"), "off");
    write(&fake.join("pub_stuck"), "");
    let (ok, out) = run(true, &["plugin", "run", "aca.tailnet.publish", "to=on"]);
    assert!(!ok && out.contains("tpub 说打开了,可现在是:关着 · 2 样"), "{out}");
    let _ = std::fs::remove_file(fake.join("pub_stuck"));
    let (ok, out) = run(true, &["plugin", "run", "aca.tailnet.publish", "to=on"]);
    assert!(ok && out.contains("发布已打开"), "{out}");
    let (_, out) = run(true, &["plugin", "view", "--json", "aca.tailnet.mounts"]);
    assert!(out.contains("发布 开着 · 2 样") && out.contains(r#""publish_to":"off""#), "{out}");
    let (ok, out) = run(false, &["plugin", "run", "aca.tailnet.publish", "to=on"]);
    assert!(!ok && out.contains("没装 tpub"), "{out}");
    // 没装 tpub:没有发布那行,p 按不了
    let (_, out) = run(false, &["plugin", "view", "--json", "aca.tailnet.mounts"]);
    assert!(!out.contains("发布") && !out.contains("publish_to"), "{out}");
}

/// 从零写一个插件:new 建好就能 link、run;check 不链接也能查;emit 让事件钩子在前台跑、输出打出来
#[test]
fn a_new_plugin_links_and_runs_check_names_problems_and_emit_runs_hooks_in_the_foreground() {
    let Some(sb) = Sandbox::new("plugdev") else { return };
    let dir = sb.home.join("work/hello");
    let d = dir.to_str().unwrap();
    let out = sb.aca(&["plugin", "new", d]);
    assert!(out.contains("(me.hello)") && out.contains("aca plugin run me.hello.hello"), "{out}");
    assert!(sb.aca(&["plugin", "check", d]).contains("✓ 没发现问题"));
    // 已经有东西的目录不动
    assert!(!sb.run(&["plugin", "new", d]).status.success());
    sb.aca(&["plugin", "link", d]);
    assert!(sb.aca(&["plugin", "run", "me.hello.hello"]).contains("你好!会话 (没有)"));
    // 钩子还注释着:没人订
    assert!(sb.aca(&["plugin", "emit", "turn.done"]).contains("没有开着的插件订了 turn.done"));
    let m = dir.join("aca-plugin.toml");
    let text = std::fs::read_to_string(&m).unwrap();
    write(&m, &text.replace("# [[events]]\n# on = \"turn.done\"\n# command = [\"sh\", \"hello.sh\"]", "[[events]]\non = \"turn.done\"\ncommand = [\"sh\", \"-c\", \"echo $ACA_PLUGIN_EVENT $ACA_PLUGIN_EVENT_JSON\"]\n\n[[events]]\non = \"turn.failed\"\nif = { kind = \"transient\" }\ncommand = [\"sh\", \"-c\", \"echo again; exit 3\"]"));
    sb.aca(&["plugin", "link", d]);
    // 事件的字段照类型放进那一行;--plugin 只跑它的
    let out = sb.aca(&["plugin", "emit", "turn.done", "secs=12", "last=好了", "--plugin", "me.hello"]);
    assert!(out.contains("--- me.hello · sh -c echo") && out.contains("退出码 0"), "{out}");
    assert!(out.contains(r#""event":"turn.done""#) && out.contains(r#""secs":12"#) && out.contains(r#""last":"好了""#), "{out}");
    // if 对不上的不跑;对得上、退出码不是 0 的,emit 的退出码也不是 0
    assert!(sb.aca(&["plugin", "emit", "turn.failed", "kind=account"]).contains("没有开着的插件订了"));
    let o = sb.run(&["plugin", "emit", "turn.failed", "kind=transient"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success() && out.contains("退出码 3") && out.contains("again"), "{out}");
    assert!(!sb.run(&["plugin", "emit", "turn.nope"]).status.success());
    // check:脚本不在、要用的程序没有,都点名;有毛病退出码是 1
    let bad = sb.home.join("work/bad");
    std::fs::create_dir_all(&bad).unwrap();
    write(
        &bad.join("aca-plugin.toml"),
        "id = \"x.bad\"\nname = \"Bad\"\nversion = \"1\"\nneeds = [\"surely-missing-prog-aca\"]\n[[commands]]\nid = \"a\"\ncommand = [\"sh\", \"gone.sh\"]\n",
    );
    let o = sb.run(&["plugin", "check", bad.to_str().unwrap()]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        !o.status.success() && out.contains("✕ 命令 a:gone.sh 在插件目录里没有") && out.contains("⚠ 要用到 surely-missing-prog-aca"),
        "{out}"
    );
    // 链接时也提醒缺的程序;aca plugin ls 也说
    let out = sb.aca(&["plugin", "link", bad.to_str().unwrap()]);
    assert!(out.contains("要用到 surely-missing-prog-aca"), "{out}");
    assert!(sb.aca(&["plugin", "ls"]).contains("⚠ x.bad:要用到 surely-missing-prog-aca"));
}

/// 插件接口(api 1)的约定:每种调法交给命令的环境变量正好是这些、文档里都写了;列表视图只读标准输出;
/// 清单里不认识的字段提醒、版本比 aca 新的不让链接、在 agent 那边撞名的不让链接
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
    // 外面带进来的(测试给 aca 的 ACA_HOME、ACA_TMUX_SOCKET)不算:插件的进程照样继承 aca 的环境
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
    // 命令行跑一条能力:冲着 worker,派它的那个在 ACA_SESSION_PARENT
    let out = sb.aca(&["plugin", "run", "t.contract.env", "--session", &worker, "word=hi"]);
    assert_eq!(names(&out), want(&command), "{out}");
    assert!(out.contains(&format!("parent={lead}")), "{out}");
    // 视图
    let out = sb.aca(&["plugin", "view", "t.contract.envview"]);
    assert_eq!(names(&out), want(&["ACA_PLUGIN_ENTRYPOINT_ID"]), "{out}");
    // 事件钩子
    let out = sb.aca(&["plugin", "emit", "turn.done", "--plugin", "t.contract", "--session", &worker]);
    assert_eq!(names(&out), want(&["ACA_PLUGIN_EVENT", "ACA_PLUGIN_EVENT_JSON"]), "{out}");
    assert!(out.contains(&format!("parent={lead}")), "{out}");
    assert!(out.contains("by=event timer=\n") || out.ends_with("by=event timer="), "{out}");
    // 定时:跟命令行跑这个能力一样的环境,没有会话,ACA_PLUGIN_TIMER 是哪个定时
    let out = sb.aca(&["plugin", "timer", "run", "t.contract.env"]);
    assert_eq!(names(&out), want(&command[1..]), "{out}");
    assert!(out.contains("parent=\n") && out.contains("by=timer timer=t.contract.env"), "{out}");
    // agent 调的工具(MCP)
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
    // 上面出现过的每一个,文档「交给命令的环境」里都有
    let docs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/plugins.md")).unwrap();
    for n in want(&["ACA_PLUGIN_EVENT", "ACA_PLUGIN_EVENT_JSON", "ACA_INPUT_FILE"])
        .iter()
        .chain(command.iter().map(|s| s.to_string()).collect::<Vec<_>>().iter())
    {
        let n = n.strip_prefix("ACA_ARG_").map(|_| "ACA_ARG_<").or(n.strip_prefix("ACA_SETTING_").map(|_| "ACA_SETTING_<")).unwrap_or(n);
        assert!(docs.contains(&format!("`{n}")), "docs/plugins.md 没写 {n}");
    }

    // 列表视图跑成了只读标准输出:标准错误写进插件的日志,不搅进 JSON
    let out = sb.aca(&["plugin", "view", "t.contract.rows"]);
    assert!(out.contains("第一条") && !out.contains("a note"), "{out}");
    let raw = sb.json(&["plugin", "view", "t.contract.rows", "--json"]);
    assert_eq!(raw["rows"][0]["id"], "a");
    assert!(sb.aca(&["plugin", "log", "t.contract"]).contains("a note on stderr"));

    // 清单里不认识的字段:链接照样链接,点名提醒(写在 [[keys]] 后面的 needs 归了那个键)
    let typo = sb.home.join("work/typo");
    write(
        &typo.join("aca-plugin.toml"),
        "id = \"t.typo\"\nname = \"Typo\"\nversion = \"1\"\ndescripton = \"x\"\n[[commands]]\nid = \"go\"\ncommand = [\"true\"]\n[[keys]]\nkey = \"g\"\nrun = \"go\"\nneeds = [\"sh\"]\n",
    );
    let out = sb.aca(&["plugin", "link", typo.to_str().unwrap()]);
    assert!(out.contains("不认识 descripton") && out.contains("不认识 keys[1].needs"), "{out}");
    assert!(sb.aca(&["plugin", "check", typo.to_str().unwrap()]).contains("⚠ 清单里不认识 keys[1].needs"));
    // 照新一版接口写的:不让链接,说先升级 aca
    let newer = sb.home.join("work/newer");
    write(&newer.join("aca-plugin.toml"), "id = \"t.newer\"\nname = \"Newer\"\nversion = \"1\"\napi = 2\n");
    let o = sb.run(&["plugin", "link", newer.to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("先升级 aca"), "{o:?}");
    // id 换成 agent 那边的名字跟已经链接的撞了(t-contract 和 t.contract 都是 t-contract):不让链接
    let twin = sb.home.join("work/twin");
    write(&twin.join("aca-plugin.toml"), "id = \"t-contract\"\nname = \"Twin\"\nversion = \"1\"\n");
    let o = sb.run(&["plugin", "link", twin.to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("同一个名字"), "{o:?}");
}

/// 你在某个会话的 tmux 里另开的窗口:不写 id 的 show / output / plugin run 冲着这个会话。
/// 只是补上没写的 id:agent 自己(环境里有 ACA_SESSION_ID)不补,新建的会话也不因此挂到它下面
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
    // 插件的命令也冲着它
    let dir = sb.home.join("work/here");
    write(
        &dir.join("aca-plugin.toml"),
        "id = \"t.here\"\nname = \"Here\"\nversion = \"1\"\n[[commands]]\nid = \"who\"\ncommand = [\"sh\", \"-c\", \"echo session=$ACA_SESSION_ID\"]\n",
    );
    sb.aca(&["plugin", "link", dir.to_str().unwrap()]);
    let o = typed(&mine, None, &["plugin", "run", "t.here.who"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), format!("session={id}"));
    // 新建的会话不挂到这个会话下面(「我是谁」还是只看 ACA_SESSION_ID)
    let o = typed(&mine, None, &["new", "fake", &proj, "-t", "child", "--hold", "--json"]);
    assert_eq!(serde_json::from_slice::<Value>(&o.stdout).unwrap()["parent"], "", "{o:?}");
    // agent 自己调的(环境里有 ACA_SESSION_ID)不补:不写 id 就是没写
    assert!(!typed(&mine, Some(&other), &["show"]).status.success());
    // 不是 aca 的 tmux 会话:没有可补的
    let plain = sb.tmux(&["new-session", "-d", "-s", "plain", "-P", "-F", "#{pane_id}"]);
    assert!(!typed(&plain, None, &["show"]).status.success());
}

/// Claude 的名字跟着会话走(`aca hook-name`):起了名的一起来就交给它,已经叫这个了不再说;
/// 在 aca 里改了名跟过去,它在 Claude 里 /rename 过就不动;别的事件、不是 aca 的会话一个字不写
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

/// tmux 状态栏那一段:在等你确认的、跑完没看的各有几个(不打名字),你正看着的那个不算,看过了就没了;只读,不动别的
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
    assert_eq!(sb.aca(&["tmux-status"]), "#[fg=red reverse bold] ! 1 #[default]");
    // 正看着它:不算
    assert_eq!(sb.aca(&["tmux-status", &id]), "");
    hook("Stop", ",\"last_assistant_message\":\"好了\",\"background_tasks\":[]");
    assert_eq!(sb.aca(&["tmux-status"]), "#[fg=brightblack]● 1#[default]");
    let events = sb.event_names().len();
    sb.aca(&["seen", &id]);
    assert_eq!(sb.aca(&["tmux-status"]), "");
    // 问了几次也没多出事件
    assert_eq!(sb.event_names().len(), events);
}

/// 插件的事件钩子没跑成:`aca plugin ls`、状态栏那一段都标出来(哪个插件、哪一处);下一轮跑成了就不提了。
/// 插件停了不提,再开着又提;重新链接(清单可能换了,第几个钩子对不上)记录清掉
#[test]
fn a_failing_hook_is_flagged_until_it_runs_fine_again() {
    let Some(sb) = Sandbox::new("health") else { return };
    let dir = sb.home.join("flaky");
    write(
        &dir.join("aca-plugin.toml"),
        "id = \"test.flaky\"\nname = \"Flaky\"\nversion = \"0.1.0\"\n[[events]]\non = \"turn.done\"\ncommand = [\"sh\", \"flaky.sh\"]\n",
    );
    // 状态目录里有 fail 这个文件就退出 3
    write(&dir.join("flaky.sh"), "[ -e \"$ACA_PLUGIN_STATE_DIR/fail\" ] && exit 3\nexit 0\n");
    sb.aca(&["plugin", "link", &dir.to_string_lossy()]);
    let fail = sb.home.join("state/plugins/test.flaky/fail");
    write(&fail, "");
    let id = sb.aca(&["new", "fake", &sb.proj(), "-t", "写文档"]);
    let pane = sb.pane(&id);
    let rec = sb.home.join("state/plugin-health/test.flaky/hook-0");
    // 一轮:说一句、答完;钩子不等,等它把退出码记下来
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
    // 正看着的那个会话不算在等你的里面:这一段只剩插件那半
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

/// aca tmux install / uninstall:aca 那份写在 aca 自己的目录里,你的 tmux.conf 只多两行(在 TPM 前面),开着的 tmux 马上用上;
/// 装两遍不重复;卸了 tmux.conf 一字不差地回到原样,开着的 tmux 里 aca 的键和状态栏那一段也撤了
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
    // 设了 ACA_HOME 时「你的 tmux.conf」是 $ACA_HOME/tmux.conf
    let conf = sb.home.join("tmux.conf");
    let original = "set -g status-right \"MINE\"\nbind a run-shell 'tmux display-popup \"aca ui --client #{client_name}\"'\n\n# ====\n# TPM\n# ====\nrun '~/.tmux/plugins/tpm/tpm'\n";
    write(&conf, original);
    assert!(sb.aca(&["tmux"]).contains("没装"));
    let out = sb.aca(&["tmux", "install"]);
    assert!(out.contains("第 5 行加了") && out.contains("开着的 tmux 已经用上了") && out.contains("第 2 行是你自己写的"), "{out}");
    let text = std::fs::read_to_string(&conf).unwrap();
    let ours = sb.home.join("config/tmux.conf");
    assert!(text.contains("\n# aca:") && text.contains(&format!("source-file -q {}\n\n# ====\n# TPM", ours.display())), "{text}");
    assert!(std::fs::read_to_string(&ours).unwrap().contains("tmux-status #{@aca_id}"));
    assert_eq!(ours_bound(), 2);
    assert_eq!(right().matches("tmux-status").count(), 1, "{}", right());
    // 再装一遍:那份重写,tmux.conf 里不多一行,状态栏不接两遍
    assert!(sb.aca(&["tmux", "install"]).contains("已经有 source 它的那一行了"));
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), text);
    assert_eq!(right().matches("tmux-status").count(), 1);
    assert!(sb.aca(&["tmux"]).contains("装了"));
    // 会话自己设了 status-right(agent-deck 建的那些):看的还是全局那份,不再接
    sb.tmux(&["set-option", "-t", &sb.pane(&id), "status-right", "这个会话自己的"]);
    assert!(!sb.aca(&["tmux", "install"]).contains("只留了一遍"));
    assert_eq!(right().matches("tmux-status").count(), 1, "{}", right());
    // 早先接重了的:装的时候收成一遍,你自己的那些原样
    let file = std::fs::read_to_string(&ours).unwrap();
    let seg = file.lines().find_map(|l| l.trim().strip_prefix("set -ag status-right \"")?.strip_suffix('"')).unwrap();
    let one = right();
    sb.tmux(&["set-option", "-ag", "status-right", &seg.repeat(3 - one.matches(seg).count())]);
    let out = sb.aca(&["tmux", "install"]);
    assert!(out.contains("原来接了 3 遍,只留了一遍"), "{out}\n{}", right());
    assert_eq!(right().matches(seg).count(), 1, "{}", right());
    assert_eq!(right().replace(seg, ""), one.replace(seg, ""));
    // 卸:原样回来
    let out = sb.aca(&["tmux", "uninstall"]);
    assert!(out.contains("两行拿掉了") && out.contains("也撤了"), "{out}");
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), original);
    assert!(!ours.exists());
    assert_eq!(ours_bound(), 0);
    assert_eq!(right(), before);
    assert!(sb.aca(&["tmux", "uninstall"]).contains("没有 source aca 那份的行"));
}

/// 根本起不来的(程序不在 PATH 里):报错说缺什么,登记撤回,列表里不留一条停着的
#[test]
fn a_session_whose_program_isnt_there_leaves_nothing_behind() {
    let Some(sb) = Sandbox::new("nolaunch") else { return };
    let cfg = sb.home.join("config/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap() + "\n[tools.gone]\nkind = \"claude\"\ncmd = [\"aca-no-such-agent\"]\n";
    std::fs::write(&cfg, text).unwrap();
    let o = sb.run(&["new", "gone", &sb.proj(), "-t", "ghost"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("PATH 里没有 aca-no-such-agent"), "{err}");
    assert_eq!(sb.json(&["ls", "--json", "--no-screen"]), serde_json::json!([]));
    let ev = sb.event_names();
    assert!(ev.ends_with(&["session.created".to_string(), "session.removed".to_string()]), "{ev:?}");
}

/// 你自己的插件:~/.config/aca/user/ 放了清单就有(不用 link),删了就没;sync 不删它,remove 删不掉它。
/// cwd = "session" 在会话目录里跑,input 把会话的最后一条回复 / 屏幕交给它(窗格读 ACA_INPUT_FILE)
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
    // 没 link:列出来就有,标着是你自己的
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("● user") && ls.contains("你自己的"), "{ls}");
    assert!(sb.aca(&["plugin", "key"]).contains("w  user.where  在哪"));
    // 在会话目录里跑,标准输入是最后一条回复
    let out = sb.aca(&["plugin", "run", "user.where", "--session", &id]);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].ends_with("/proj") && lines[1] == "看 https://x.dev/a", "{out}");
    // 屏幕上的字
    assert!(sb.aca(&["plugin", "run", "user.screen", "--session", &id]).contains("fake agent up"));
    // 窗格:开在会话的新窗口里,内容在 ACA_INPUT_FILE;./dump.sh 照样按插件目录找
    sb.aca(&["plugin", "pane", "user", "dump", "--session", &id, "--placement", "window"]);
    let dumped = sb.home.join("state/plugins/user/pane.txt");
    assert!(sb.until(|| std::fs::read_to_string(&dumped).is_ok_and(|t| t.contains("x.dev"))), "窗格没跑");
    let t = std::fs::read_to_string(&dumped).unwrap();
    assert!(t.lines().next().unwrap().ends_with("/proj"), "{t}");
    // sync 不删它;remove 删不掉,说怎么办
    sb.aca(&["plugin", "sync"]);
    assert!(sb.aca(&["plugin", "ls"]).contains("● user"));
    let o = sb.run(&["plugin", "remove", "user"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("是你自己的"), "{}", String::from_utf8_lossy(&o.stderr));
    // 停着:plugins.toml 里记下,菜单里没了
    sb.aca(&["plugin", "disable", "user"]);
    assert!(sb.aca(&["plugin", "ls"]).contains("○ user"));
    assert!(!sb.aca(&["plugin", "key"]).contains("user.where"));
    sb.aca(&["plugin", "enable", "user"]);
    // 写坏了:列出来说哪不对
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(&manifest, "[[commands]]\nid = \"x\"\n");
    let ls = sb.aca(&["plugin", "ls"]);
    assert!(ls.contains("user") && ls.contains("清单有问题") && ls.contains("command"), "{ls}");
    // 删了就没了
    std::fs::remove_file(&manifest).unwrap();
    assert!(!sb.aca(&["plugin", "ls"]).contains("user"));
}

/// 插件能力服务列给 agent 的工具名(`aca plugin tools`,没有会话)
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

/// 你说了算:写 agent? 的能力你点了名才给 agent;: 菜单里的键你能改、能去掉,两个插件抢一个键时点名
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
    // agent? 的默认不给
    let (names, _) = agent_tools(&sb, None);
    assert_eq!(names, ["t-perm__ls"]);
    let choices = sb.aca(&["plugin", "allow", "t.perm"]);
    assert!(choices.contains("✓ ls") && choices.contains("○ add  要你点名才给"), "{choices}");
    // 点名给;收回 ls:写进 plugins.toml,能力服务照它列、照它调
    sb.aca(&["plugin", "allow", "t.perm", "add"]);
    assert_eq!(agent_tools(&sb, None).0, ["t-perm__ls", "t-perm__add"]);
    let out = sb.aca(&["plugin", "deny", "t.perm", "ls"]);
    assert!(out.contains("○ ls") && out.contains("✓ add") && out.contains("重启"), "{out}");
    assert!(std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("tools = [\"add\"]"));
    let (names, said) = agent_tools(&sb, Some("t-perm__ls"));
    assert_eq!(names, ["t-perm__add"]);
    assert!(said.contains("没有 t-perm__ls 这个工具"), "{said}");
    assert_eq!(agent_tools(&sb, Some("t-perm__add")).1.trim(), "add");
    // 清单里没写给 agent 的给不了;--reset 回到清单的
    let o = sb.run(&["plugin", "allow", "t.other", "x"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("不能给 agent"));
    sb.aca(&["plugin", "allow", "t.perm", "--reset"]);
    assert_eq!(agent_tools(&sb, None).0, ["t-perm__ls"]);
    assert!(!std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("tools ="));

    // 两个插件的清单都要 m:先登记的拿到,另一个点名(列插件时也说)
    let keys = sb.aca(&["plugin", "key"]);
    assert!(keys.contains("m  t.perm.ls  列") && keys.contains("⚠ 键 m 给了 t.perm「列」,t.other「叉」没拿到"), "{keys}");
    assert!(sb.aca(&["plugin", "ls"]).contains("⚠ 键 m 给了"));
    // 给 t.other 换一个;把 t.perm 的去掉;再把 m 给 t.other
    assert!(sb.aca(&["plugin", "key", "t.other", "x", "M"]).contains("按 M"));
    assert!(!sb.aca(&["plugin", "key"]).contains("⚠"));
    assert!(sb.aca(&["plugin", "key", "t.perm", "ls", "--none"]).contains("没有键"));
    sb.aca(&["plugin", "key", "t.other", "x", "m"]);
    let keys = sb.aca(&["plugin", "key"]);
    assert!(keys.contains("m  t.other.x") && !keys.contains("t.perm.ls"), "{keys}");
    // 你写的先拿:t.perm 回到清单的 m,也拿不到
    sb.aca(&["plugin", "key", "t.perm", "ls", "--reset"]);
    let keys = sb.aca(&["plugin", "key"]);
    assert!(keys.contains("m  t.other.x") && keys.contains("⚠ 键 m 给了 t.other「叉」,t.perm「列」没拿到"), "{keys}");
    // 不能用的键、没有的目标
    let o = sb.run(&["plugin", "key", "t.perm", "ls", "/"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains(": 菜单自己用的"));
    let o = sb.run(&["plugin", "key", "t.perm", "nope", "z"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("没有 nope"));
}

/// 热键:插件开着就绑在 tmux 上(和 Ctrl+Q 一个办法,只在 aca 的会话里拦),换键、停掉、删掉跟着变;
/// 你自己绑的键不动;tmux 重启了,下次建会话时补上
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
    // 键表里 aca 替插件绑的热键:「键表:键」(aca 自己的那个键 —— 进下一个在等你的 —— 不算,它有自己的一条测试)
    let bound = || {
        let mut v: Vec<String> = sb
            .tmux(&["list-keys"])
            .lines()
            .filter(|l| l.contains("--hotkey") && l.contains(" plugin pane "))
            .filter_map(|l| {
                let mut w = l.split_once("-T")?.1.split_whitespace();
                Some(format!("{}:{}", w.next()?, w.next()?))
            })
            .collect();
        v.sort();
        v
    };
    let all = |k: &str| vec![format!("copy-mode-vi:{k}"), format!("copy-mode:{k}"), format!("root:{k}")];
    assert!(bound().is_empty());
    // 链接上就绑:平常的键表,和翻着看的两张
    sb.aca(&["plugin", "link", dir.to_str().unwrap()]);
    assert_eq!(bound(), all("C-M-y"));
    let root = sb.tmux(&["list-keys", "-T", "root"]);
    let line = root.lines().find(|l| l.contains("--hotkey") && l.contains(" plugin pane ")).unwrap();
    // 只在 aca 的会话里拦,别的会话里把键送进去;按了开的是这个插件的这个窗格,冲着按键的会话和终端
    assert!(line.contains("#{?@aca_id,1,0}") && line.contains("send-keys C-M-y"), "{line}");
    assert!(line.contains("plugin pane t.hot pad --hotkey C-M-y --session '#{@aca_id}' --client '#{client_name}'"), "{line}");
    assert_eq!(sb.aca(&["plugin", "hotkey"]), "C-M-y  t.hot.pad  便签");
    // 那条命令真跑起来:窗格拿到会话的最后一条回复
    sb.aca(&["plugin", "pane", "t.hot", "pad", "--hotkey", "C-M-y", "--session", &id, "--placement", "window"]);
    let got = sb.home.join("state/plugins/t.hot/got.txt");
    assert!(sb.until(|| std::fs::read_to_string(&got).is_ok_and(|t| t.contains("make test"))), "窗格没跑");
    // 换键:旧的三处都解了;写进 plugins.toml。清单没给热键的窗格也能给一个;同一个键第二个拿不到,点名
    assert!(sb.aca(&["plugin", "hotkey", "t.hot", "pad", "C-^"]).contains("在会话里按 C-^"));
    assert_eq!(bound(), all("C-^"));
    let decl = std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap();
    assert!(decl.contains("hotkeys = { pad = \"C-^\" }"), "{decl}");
    let out = sb.aca(&["plugin", "hotkey", "t.hot", "other", "C-^"]);
    assert!(out.contains("没有热键") && out.contains("键 C-^ 给了 t.hot「便签」"), "{out}");
    assert!(sb.aca(&["plugin", "ls"]).contains("键 C-^ 给了 t.hot「便签」"));
    sb.aca(&["plugin", "hotkey", "t.hot", "other", "--reset"]);
    // 写得不对的不收:光一个字符的、aca 自己的、不是窗格的
    for (pane, key, why) in [("pad", "y", "tmux"), ("pad", "C-q", "C-q"), ("pad", "C-M-c", "C-M-c"), ("nope", "M-y", "没有 nope")] {
        let o = sb.run(&["plugin", "hotkey", "t.hot", pane, key]);
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains(why), "{pane} {key}:{o:?}");
    }
    assert_eq!(bound(), all("C-^"));
    // 那个键你自己在 tmux 里绑了别的:不动你的,点名;翻着看的那两张里你没绑,照绑
    sb.tmux(&["bind-key", "-n", "F5", "display-message", "mine"]);
    let out = sb.aca(&["plugin", "hotkey", "t.hot", "pad", "F5"]);
    assert!(out.contains("键 F5 你在 tmux 里绑了别的,没动它"), "{out}");
    assert!(sb.tmux(&["list-keys", "-T", "root"]).lines().any(|l| l.contains("F5") && l.contains("display-message mine")));
    assert_eq!(bound(), ["copy-mode-vi:F5", "copy-mode:F5"]);
    // 回到清单的;不要键;停掉 / 再开
    sb.aca(&["plugin", "hotkey", "t.hot", "pad", "--reset"]);
    assert_eq!(bound(), all("C-M-y"));
    assert!(sb.aca(&["plugin", "hotkey", "t.hot", "pad", "--none"]).contains("没有热键"));
    assert!(bound().is_empty());
    sb.aca(&["plugin", "hotkey", "t.hot", "pad", "--reset"]);
    sb.aca(&["plugin", "disable", "t.hot"]);
    assert!(bound().is_empty());
    // 停着的时候按到了上回留下的键(这里手绑一条装作没解掉的):不报错,顺手解掉
    sb.tmux(&["bind-key", "-n", "C-^", "run-shell", "aca plugin pane t.hot pad --hotkey C-^"]);
    let o = sb.run(&["plugin", "pane", "t.hot", "pad", "--hotkey", "C-^", "--session", &id]);
    assert!(o.status.success() && o.stdout.is_empty() && o.stderr.is_empty(), "{o:?}");
    assert!(bound().is_empty());
    sb.aca(&["plugin", "enable", "t.hot"]);
    assert_eq!(bound(), all("C-M-y"));
    // tmux 服务重启(绑定不在 tmux.conf 里):再建一个会话就补上
    sb.tmux(&["kill-server"]);
    sb.aca(&["new", "fake", &sb.proj(), "-t", "第二个"]);
    assert_eq!(bound(), all("C-M-y"));
    // 不用了:键跟着没了
    sb.aca(&["plugin", "unlink", "t.hot"]);
    assert!(bound().is_empty());
    // 你自己的那份(不用 link):清单里写了热键,下次用到 aca 就绑上;那份删了就解掉
    let mine = sb.home.join("config/user/aca-plugin.toml");
    write(&mine, "[[panes]]\nid = \"lg\"\ncommand = [\"true\"]\n[[hotkeys]]\nkey = \"M-=\"\nrun = \"lg\"\n");
    assert_eq!(sb.aca(&["plugin", "hotkey"]), "M-=  user.lg  lg");
    assert_eq!(bound(), all("M-="));
    std::fs::remove_file(&mine).unwrap();
    sb.aca(&["plugin", "ls"]);
    assert!(bound().is_empty());
}

/// 状态栏上只打有几个在等你(`!` 在问的、`●` 跑完没看的);aca 绑的那个键(config.toml 的 `[keys] next`,默认 Ctrl+Alt+N)
/// 在会话里真按一下,这台客户端就进下一个:先在问的,再跑完没看的;都看过了在问的之间轮;一个都没有就说一句。
/// 键能换、能不要,你自己绑了别的不动
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
    // 状态栏:种类和个数,在问的是红底的一块,跑完的是次要的字色;没有名字。你正看着的那个不算
    let bar = |id: &str| sb.aca(&["tmux-status", id]);
    assert_eq!(bar(&here), "#[fg=red reverse bold] ! 1 #[default] #[fg=brightblack]● 1#[default]");
    assert_eq!(bar(&ask), "#[fg=brightblack]● 1#[default]");

    // 键:建会话时就绑上了,三张键表;只在 aca 的会话里拦,别处键原样送进去
    let keys = |k: &str| sb.tmux(&["list-keys"]).lines().filter(|l| l.contains(" next --hotkey ") && l.contains(&format!(" {k} "))).count();
    assert_eq!(keys("C-M-n"), 3);
    let root = sb.tmux(&["list-keys", "-T", "root"]);
    let line = root.lines().find(|l| l.contains(" next --hotkey ")).unwrap();
    assert!(line.contains("#{?@aca_id,1,0}") && line.contains("send-keys C-M-n"), "{line}");
    assert!(line.contains("next --hotkey C-M-n --session '#{@aca_id}' --client '#{client_name}'"), "{line}");

    // 一台真的客户端:另一个 tmux 服务的窗格里连进来,键从那个窗格按进去
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
    // 先在问的
    press("C-M-n");
    assert!(sb.until(|| at() == ask), "没进在问的那个:{}", at());
    // 再按:跑完没看的。进去过就算看过了
    press("C-M-n");
    assert!(sb.until(|| at() == done), "没进跑完的那个:{}", at());
    assert!(sb.until(|| bar(&here) == "#[fg=red reverse bold] ! 1 #[default]"), "{}", bar(&here));
    // 再按:没看过的没有了,还有一个在问,回到它;在它上面再按,一个都没有了,人不动、说一句
    press("C-M-n");
    assert!(sb.until(|| at() == ask), "没回到在问的那个:{}", at());
    press("C-M-n");
    assert!(sb.until(|| sb.tmux(&["show-messages"]).contains("没有在等你的")), "{}", sb.tmux(&["show-messages"]));
    assert_eq!(at(), ask);
    out(&["kill-server"]);

    // 自己敲也行(不在 tmux 里、没有终端:连不上去,说进不去;不算看过)
    let o = sb.run(&["next", "--session", &ask]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("没有在等你的"), "{o:?}");

    // 换键:旧的解掉、新的绑上,记在 config.toml;写得不能当热键的不收
    let out = sb.aca(&["tmux", "key", "next", "M-n"]);
    assert!(out.contains("在会话里按 Alt+N(M-n)"), "{out}");
    assert_eq!((keys("C-M-n"), keys("M-n")), (0, 3));
    assert!(std::fs::read_to_string(sb.home.join("config/config.toml")).unwrap().contains("[keys]\nnext = \"M-n\""));
    let o = sb.run(&["tmux", "key", "next", "n"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("热键要写成 tmux 的键名"), "{o:?}");
    assert_eq!(keys("M-n"), 3);
    // 那个键你自己在 tmux 里绑了别的:不动你的,点名
    sb.tmux(&["bind-key", "-n", "F7", "display-message", "mine"]);
    let out = sb.aca(&["tmux", "key", "next", "F7"]);
    assert!(out.contains("键 F7 你在 tmux 里绑了别的,没动它"), "{out}");
    assert!(sb.tmux(&["list-keys", "-T", "root"]).lines().any(|l| l.contains(" F7 ") && l.contains("display-message mine")));
    // 不要键:都解掉,aca tmux 里说没绑;aca next 照样能敲
    let out = sb.aca(&["tmux", "key", "next", "--none"]);
    assert!(out.contains("没绑键"), "{out}");
    assert_eq!(sb.tmux(&["list-keys"]).lines().filter(|l| l.contains(" next --hotkey ")).count(), 0);
    assert!(sb.aca(&["tmux"]).contains("没绑键"));
}

/// Ctrl+Q:你人在会话里时 agent-deck 又起一个会话,它那一版(不认 aca 的会话,键被吞掉)一绑上来,tmux 当场换回 aca 的 ——
/// 挂在 `after-bind-key` 上的钩子。只占数组里自己那一格;你自己把这个键绑了别的,不动
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
    // agent-deck 起会话时绑的那一条,两种写法
    for table in [&["-n"][..], &["-T", "root"]] {
        sb.tmux(&[&["bind-key"], table, &["C-q"], &deck[..]].concat());
        assert!(ours(&bound()), "{table:?}:{}", bound());
    }
    // 别的键、aca 自己再绑一遍:不关钩子的事
    sb.tmux(&[&["bind-key", "-n", "F6"], &deck[..]].concat());
    assert!(sb.tmux(&["list-keys", "-T", "root"]).lines().any(|l| l.contains(" F6 ") && !l.contains("@aca_id")));
    // 同一个钩子里你挂的另一格留着;aca 再来几遍还是只占一格
    sb.tmux(&["set-hook", "-g", "after-bind-key[7]", "set-option -g @mine 1"]);
    sb.aca(&["new", "fake", &sb.proj(), "-t", "第二个"]);
    let h = hooks();
    assert!(h.len() == 2 && h.iter().any(|l| l.contains("@mine")) && h.iter().any(|l| l.contains("hook_argument_0")), "{h:?}");
    // 你自己把这个键绑了别的:钩子不抢,下次建会话也不抢
    sb.tmux(&["bind-key", "-n", "C-q", "display-message", "mine"]);
    assert!(bound().contains("display-message mine"), "{}", bound());
    sb.aca(&["new", "fake", &sb.proj(), "-t", "第三个"]);
    assert!(bound().contains("display-message mine"), "{}", bound());
    // tmux 服务重启:键和钩子都没了,再建一个会话一起补上
    sb.tmux(&["kill-server"]);
    assert!(hooks().is_empty());
    sb.aca(&["new", "fake", &sb.proj(), "-t", "第四个"]);
    assert!(ours(&bound()) && hooks().len() == 1, "{} {:?}", bound(), hooks());
    sb.tmux(&[&["bind-key", "-n", "C-q"], &deck[..]].concat());
    assert!(ours(&bound()), "{}", bound());
}

/// aca.copy:从最后一条回复的原文里认出命令(代码块、shell 代码块里的每一条、带空格的行内代码);
/// hook 没记到回复的会话(接管来还没重启的),aca 去它的对话记录里读
#[test]
fn the_copy_plugin_finds_the_commands_in_the_last_reply_and_reads_the_transcript_when_no_hook_recorded_it() {
    // 沙盒的名字里别带 copy:最后按这四个字母查删干净了没有
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
    // 还没有回复
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
    // 设置:行内的都要;代码块里的不拆
    sb.aca(&["plugin", "setting", "aca.copy", "inline", "all"]);
    sb.aca(&["plugin", "setting", "aca.copy", "lines", "false"]);
    let out = sb.aca(&["plugin", "run", "aca.copy.list", "--session", &id]);
    assert!(out.contains("4  --apply") && !out.contains("└"), "{out}");
    // 换了一段对话,hook 只报了记录在哪、还没报过回复:从记录里读最后一段
    let t = sb.home.join("t.jsonl");
    let said = serde_json::json!({ "type": "assistant", "message": { "model": "m", "content": [{ "type": "text", "text": "装上:\n\n```sh\naca plugin install aca.copy\n```" }] } });
    write(&t, &format!("{said}\n"));
    let id2 = sb.aca(&["new", "fake", &sb.proj(), "-t", "没 hook 的"]);
    let pane2 = sb.pane(&id2);
    let prompt = serde_json::json!({ "hook_event_name": "UserPromptSubmit", "session_id": "c2", "transcript_path": t, "prompt": "hi" });
    sb.hook(&id2, &pane2, &prompt.to_string());
    assert_eq!(sb.aca(&["output", &id2]), "");
    assert_eq!(sb.aca(&["plugin", "run", "aca.copy.list", "--session", &id2]).trim(), "1  aca plugin install aca.copy");
    // 窗格开过一回:aca 把回复存了一份交给它
    sb.aca(&["plugin", "setting", "aca.copy", "copy", "cat > /dev/null"]);
    sb.aca(&["plugin", "pane", "aca.copy", "pick", "--session", &id2, "--placement", "window"]);
    let input = sb.home.join("state/plugin-input/aca.copy.pick.txt");
    assert!(input.exists());
    // 删掉:键跟着没了;本机上不留它的东西(装的那份、plugins.toml 里那一段、存的那份回复、aca 替它建的空目录)
    sb.aca(&["plugin", "remove", "aca.copy"]);
    assert!(!sb.tmux(&["list-keys"]).contains(" plugin pane "));
    let left: Vec<String> =
        ["config", "state", "data"].iter().flat_map(|d| walk(&sb.home.join(d))).filter(|p| p.contains("copy")).collect();
    assert!(left.is_empty(), "{left:?}");
    assert!(!std::fs::read_to_string(sb.home.join("config/plugins.toml")).unwrap().contains("aca.copy"));
}

/// aca.notify:跑完了、在等你、出错了弹一条桌面通知。弹的那条命令换成沙盒里一个只记下来的脚本(测试里不真弹):
/// 标题是记号加会话名,正文是去掉 markdown 记号的回复;派出去的 worker 开了设置才弹;你换的命令发不出去就用系统的
#[test]
fn the_notify_plugin_shows_a_desktop_notification_for_your_sessions_and_skips_workers() {
    let Some(sb) = Sandbox::new("notify") else { return };
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("没有 jq,跳过");
        return;
    }
    let out = sb.aca(&["plugin", "install", "aca.notify"]);
    assert!(out.contains("装好了 aca.notify") && out.contains("turn.done"), "{out}");
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
    // 派出去的 worker:默认不弹(它跑完会告诉派它的那个),开了设置才弹
    let worker = sb.aca(&["new", "fake", &sb.proj(), "-t", "干活的", "--parent", &lead]);
    emit(&worker, "turn.done", "last=做完了");
    assert_eq!(shown().lines().count(), 3, "{}", shown());
    sb.aca(&["plugin", "setting", "aca.notify", "workers", "true"]);
    emit(&worker, "turn.done", "last=做完了");
    assert_eq!(shown().lines().last(), Some("✓ 干活的|做完了"));
    // 菜单里的「弹一条试试」
    assert!(sb.aca(&["plugin", "run", "aca.notify.test"]).contains("shown"));
    assert_eq!(shown().lines().last(), Some("aca|Notify works"));
    // 你换的命令发不出去(退出码不是 0):照空着的办,用系统的(这里是 PATH 里排在前面的假 osascript / notify-send)
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

/// 目录底下所有的路径(文件和目录)
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

/// 往配置里再加一段
fn add_config(sb: &Sandbox, more: &str) {
    let cfg = sb.home.join("config/config.toml");
    write(&cfg, &(std::fs::read_to_string(&cfg).unwrap() + more));
}

#[test]
fn the_opening_message_goes_in_as_a_launch_argument_and_only_the_first_time() {
    let Some(sb) = Sandbox::new("opening") else { return };
    // 假 agent:把收到的参数记下来(一个参数一段,\0 隔开),起一次记一份
    let args = sb.home.join("args.bin");
    let script = sb.home.join("rec.sh");
    write(
        &script,
        &format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\0' \"$a\"; done > '{}'\necho recorded\nexec sleep 600\n", args.display()),
    );
    // rec 按 Claude 那一家起(认开场那句话当启动参数),plain 是声明式的(不认)
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

    // 一份 brief:横线开头、多行、带引号,三万多字节 —— tmux 一条命令放不下这么长,所以是经文件带进去的
    let brief = format!("- 第一条 \"带引号\" 'x'\n# Task\n{}\n最后一行", "改这个文件 ".repeat(2500));
    assert!(brief.len() > 30_000);
    let t = std::time::Instant::now();
    let o = sb.run(&["new", "rec", &sb.proj(), "-t", "带话", "--say", &brief]);
    assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
    // 不等它起来、空下来(打字进去的那条路要等)
    assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
    let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let pane = sb.pane(&id);
    assert!(sb.wait_screen(&pane, "recorded").contains("recorded"));
    let a = got();
    assert_eq!(a[a.len() - 2..], ["--".to_string(), brief.clone()]);
    // 那份文件读完就删
    assert!(walk(&sb.home.join("state/opening")).is_empty());

    // 再起:不把那句话再说一遍
    sb.aca(&["stop", &id]);
    std::fs::remove_file(&args).unwrap();
    sb.aca(&["start", &id]);
    assert!(sb.until(|| args.exists()));
    assert!(sb.wait_screen(&pane, "recorded").contains("recorded"));
    assert!(got().iter().all(|x| *x != brief && x != "--"), "{:?}", got().last());

    // 单独一个英文词会被当成子命令:不带,等它起来打字进去
    let o = sb.run(&["new", "rec", &sb.proj(), "-t", "一个词", "--say", "resume"]);
    assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
    let word = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert!(got().iter().all(|x| x != "resume" && x != "--"), "{:?}", got());
    assert!(sb.wait_screen(&sb.pane(&word), "resume").contains("resume"));

    // 不认的那一家:也是打字进去
    let o = sb.run(&["new", "plain", &sb.proj(), "-t", "打字", "--say", "看一下 README"]);
    assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
    let typed = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert!(got().iter().all(|x| x != "看一下 README" && x != "--"), "{:?}", got());
    assert!(sb.wait_screen(&sb.pane(&typed), "看一下 README").contains("看一下 README"));

    // 只登记没启动:那句话没处带,说一声,会话照样建好
    let o = sb.run(&["new", "rec", &sb.proj(), "-t", "先不起", "--hold", "--say", "两个 词"]);
    assert!(
        o.status.success() && String::from_utf8_lossy(&o.stderr).contains("第一句话没发进去"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(sb.json(&["show", "先不起", "--json"])["state"], "stopped");
}

#[test]
fn the_folder_trust_question_is_answered_at_launch_only_when_the_switch_is_on() {
    let Some(sb) = Sandbox::new("trust") else { return };
    // 假 agent:摆出 Claude 问「信不信任这个目录」的框(光标停在 No 上),读到回车为止,把按的键记下来
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

    // 开关没开:停在那等你
    let off = sb.aca(&["new", "gate", &sb.proj(), "-t", "没开"]);
    assert!(sb.wait_screen(&sb.pane(&off), "Enter to confirm").contains("Yes, I trust this folder"));
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert!(!keys.exists());
    assert_eq!(sb.json(&["show", &off, "--json"])["state"], "ask");
    sb.aca(&["rm", &off]);

    // 开了:起来之后替你从 No 走到 Yes 再回车
    add_config(&sb, "[trust]\nfolder = true\n");
    let on = sb.aca(&["new", "gate", &sb.proj(), "-t", "开了"]);
    assert!(sb.until(|| keys.exists()), "{}", sb.tmux(&["capture-pane", "-p", "-t", &sb.pane(&on)]));
    assert_eq!(std::fs::read_to_string(&keys).unwrap().trim(), "down-enter");
    let pane = sb.pane(&on);
    assert!(sb.wait_screen(&pane, "trusted").contains("trusted"));

    // 再起(同一格原地):一样替你答
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
    // 单独一个英文词:scr(按 Claude 那一家)也只能打字进去。`aca new` 要等到打进去才回,放到一边跑
    let mut new =
        sb.cmd().args(["new", "scr", &sb.proj(), "-t", "等画稳", "--say", "resume"]).stderr(std::process::Stdio::piped()).spawn().unwrap();
    // 那一头正在写登记表时这边会读不到(锁着):当成还没有,再问
    let listed = || {
        let rows: Value = serde_json::from_slice(&sb.run(&["ls", "--json"]).stdout).unwrap_or_default();
        rows.as_array().and_then(|a| a.first()).and_then(|r| r["id"].as_str()).unwrap_or("").to_string()
    };
    assert!(sb.until(|| !listed().is_empty()));
    let id = listed();
    let pane = sb.pane(&id);
    // 启动时的那道关:算「在等你」,不打
    sb.set_screen(&id, &pane, " Accessing workspace:\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(new.try_wait().unwrap().is_none());
    // 有人答掉了:它不在问了,界面却还在一帧一帧往外画。这时候打进去的字没人收(这里:下一帧一画就被擦掉)
    for i in 0..14 {
        write(&sb.home.join("screens").join(&id), &format!("Loading… {i}"));
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    // 画稳了、空着:这才打
    write(&sb.home.join("screens").join(&id), "● ready\n\n❯ ");
    let o = new.wait_with_output().unwrap();
    assert!(o.status.success() && o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
    let screen = sb.wait_screen(&pane, "resume");
    assert!(screen.contains("ready") && screen.contains("resume"), "{screen}");
}
