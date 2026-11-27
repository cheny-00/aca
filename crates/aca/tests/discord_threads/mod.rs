use super::{Sandbox, add_config, session_events, write};
use serde_json::{Value, json};
use std::process::{Command, Stdio};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

struct Discord<'a>(&'a Sandbox);

impl Discord<'_> {
    fn install(sb: &Sandbox) -> Discord<'_> {
        let curl = sb.home.join("fakebin/curl");
        write(&curl, include_str!("curl"));
        fs::set_permissions(curl, fs::Permissions::from_mode(0o755)).unwrap();
        let discord = Discord(sb);
        discord.run(&["plugin", "install", "aca.discord"]);
        for (key, value) in [("token", "T0KEN"), ("channel", "111")] {
            discord.setting(key, value);
        }
        discord
    }

    fn run(&self, args: &[&str]) -> String {
        let path = format!("{}/fakebin:{}", self.0.home.display(), std::env::var("PATH").unwrap());
        let out = self.0.cmd().env("PATH", path).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    fn setting(&self, key: &str, value: &str) {
        self.run(&["plugin", "setting", "aca.discord", key, value]);
    }

    fn emit(&self, event: &str, id: &str, fields: &[&str]) {
        let mut args = vec!["plugin", "emit", event, "--plugin", "aca.discord", "--session", id];
        args.extend_from_slice(fields);
        self.run(&args);
    }

    fn calls(&self) -> Vec<Value> {
        let file = self.0.home.join("calls");
        let log = fs::read_to_string(&file).unwrap_or_default();
        write(&file, "");
        log.lines().map(|s| serde_json::from_str(s).unwrap()).collect()
    }

    fn record(&self, id: &str) -> PathBuf {
        self.0.home.join("state/plugins/aca.discord/threads-111").join(id)
    }

    fn thread(&self, id: &str) -> String {
        fs::read_to_string(self.record(id)).unwrap()
    }

    fn answer(&self, method: &str, path: &str, body: Value) -> PathBuf {
        let file = self.0.home.join("answers").join(format!("{method}-{}", path.replace('/', "_")));
        write(&file, &body.to_string());
        file
    }
}

fn desk(sb: &Sandbox) -> String {
    add_config(sb, &format!("[scratch]\ndir = {:?}\n", sb.proj()));
    let id = sb.aca(&["new", "fake", &sb.proj(), "-g", "dispatch", "-t", "reception"]);
    sb.aca(&["group", "dir", "dispatch", &sb.proj()]);
    sb.aca(&["group", "router", "dispatch", &id]);
    id
}

#[test]
fn bot_posts_to_the_desk_or_its_sessions_thread_and_follows_its_lifetime() {
    let Some(sb) = Sandbox::new("discord-threads-post") else { return };
    let desk = desk(&sb);
    let name = "题".repeat(101);
    let worker = sb.aca(&["new", "fake", &sb.proj(), "-t", &name, "--parent", &desk, "--hold"]);
    let discord = Discord::install(&sb);
    let mut client = Command::new("tmux")
        .args(["-L", &sb.socket, "-C", "attach-session", "-t", &sb.pane(&desk)])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    assert!(sb.until(|| sb.json(&["show", &desk, "--json"])["attached"] == 1));
    discord.emit("turn.done", &desk, &["last=watched locally"]);
    assert!(discord.calls().is_empty());
    discord.emit("turn.done", &desk, &["via=Discord", "last=ready"]);
    assert_eq!(discord.calls(), [json!(["POST", "channels/111/messages", {"content":"ready","allowed_mentions":{"parse":[]}}])]);
    drop(client.stdin.take());
    assert!(client.wait().unwrap().success());
    discord.emit("turn.done", &worker, &["last=hidden worker"]);
    discord.emit("session.titled", &worker, &["title=before first post"]);
    assert!(discord.calls().is_empty());
    discord.setting("workers", "true");
    let long = "汉".repeat(2400);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| discord.emit("turn.done", &worker, &[&format!("last={long}")]));
        discord.emit("turn.done", &worker, &["last=second"]);
        a.join().unwrap();
    });
    let thread = discord.thread(&worker);
    let path = format!("channels/{thread}/messages");
    let calls = discord.calls();
    assert_eq!(calls[0], json!(["POST", "channels/111/threads", {"type":11,"name":"题".repeat(100),"auto_archive_duration":1440}]));
    assert_eq!(calls.len(), 4, "{calls:?}");
    assert!(calls[1..].iter().all(|c| c[0] == "POST" && c[1] == path && c[2].get("username").is_none()));
    let chunks: Vec<_> = calls[1..].iter().map(|c| c[2]["content"].as_str().unwrap()).filter(|s| *s != "✓ second").collect();
    assert_eq!(chunks, [format!("✓ {}", "汉".repeat(1998)), "汉".repeat(402)]);
    discord.setting("mention", "42");
    discord.setting("workers", "false");
    discord.emit("agent.waiting", &worker, &["via=Discord", "ask=Which?"]);
    assert_eq!(discord.calls(), [json!(["POST",path,{"content":"? <@42> Which?","allowed_mentions":{"parse":[],"users":["42"]}}])]);
    discord.emit("turn.done", &desk, &["last=local turn"]);
    let other = discord.thread(&desk);
    assert_ne!(other, thread);
    assert_eq!(discord.calls()[1][1], format!("channels/{other}/messages"));

    // A missing channel is recoverable from either the HTTP status or the Discord error code.
    let mut current = thread;
    for status_only in [false, true] {
        let old = format!("channels/{current}/messages");
        let body = if status_only { json!({}) } else { json!({"code":10003,"message":"Unknown Channel"}) };
        let answer = discord.answer("POST", &old, body);
        if status_only {
            write(&answer.with_extension("status"), "404");
        }
        discord.emit("turn.failed", &worker, &["via=Discord", "error=quota"]);
        let replacement = discord.thread(&worker);
        assert_ne!(current, replacement);
        let calls = discord.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!((&calls[0][1], &calls[1][1]), (&json!(old), &json!("channels/111/threads")));
        assert_eq!(
            calls[2],
            json!(["POST",format!("channels/{replacement}/messages"),{"content":"✕ quota","allowed_mentions":{"parse":[]}}])
        );
        current = replacement;
    }
    let channel = format!("channels/{current}");
    for (event, title, expected) in [("session.renamed", "title=changed", "changed"), ("session.titled", "title=", worker.as_str())] {
        discord.emit(event, &worker, &[title]);
        assert_eq!(discord.calls(), [json!(["PATCH",channel,{"name":expected}])]);
    }
    discord.emit("session.archived", &worker, &[]);
    assert_eq!(discord.calls(), [json!(["PATCH",channel,{"archived":true}])]);
    assert_eq!(discord.thread(&worker), current);
    discord.setting("url", "https://example.invalid/old-webhook");
    discord.emit("turn.done", &desk, &["via=Discord", &format!("last={}", "🌱".repeat(4005))]);
    let part = json!(["POST","channels/111/messages",{"content":"🌱".repeat(2000),"allowed_mentions":{"parse":[]}}]);
    assert_eq!(discord.calls(), [part.clone(), part]);
    discord.emit("session.renamed", &worker, &["title=quiet"]);
    assert_eq!(
        discord.calls(),
        [
            json!(["PATCH",channel,{"name":"quiet"}]),
            json!(["PATCH",channel,{"name":"quiet","archived":false}]),
            json!(["PATCH",channel,{"archived":true}]),
        ]
    );
    discord.run(&["rm", &worker]);
    assert!(sb.until(|| !discord.record(&worker).exists()));
    assert_eq!(discord.calls(), [json!(["PATCH",channel,{"archived":true}])]);
}

#[test]
fn bot_reads_new_messages_only_in_changed_known_threads_and_reports_a_missing_session_once() {
    let Some(sb) = Sandbox::new("discord-threads-pull") else { return };
    let desk = desk(&sb);
    let worker = sb.aca(&["new", "fake", &sb.proj(), "-t", "working"]);
    let discord = Discord::install(&sb);
    discord.setting("user", "42");
    for id in [&worker, &desk] {
        discord.emit("turn.done", id, &["last=finished"]);
    }
    let thread = discord.thread(&worker);
    let idle = discord.thread(&desk);
    let created: u64 = thread.parse().unwrap();
    let msg = |id: u64, user: &str, bot: bool, content: &str| json!({"id":id.to_string(),"author":{"id":user,"bot":bot},"content":content});
    let active = |last: u64| {
        discord.answer(
            "GET",
            "guilds/222/threads/active",
            json!({"threads":[
                {"id":thread,"parent_id":"111","last_message_id":last.to_string()},
                {"id":idle,"parent_id":"111","last_message_id":idle},
                {"id":"999","parent_id":"111","last_message_id":"1000"}
            ]}),
        );
    };
    let pull = || discord.run(&["plugin", "run", "aca.discord.pull"]);
    discord.answer("GET", "channels/111", json!({"guild_id":"222"}));
    discord.answer("GET", "channels/111/messages?limit=1", json!([msg(100, "42", false, "old")]));
    active(created);
    discord.calls();
    pull();
    let urls = |calls: Vec<Value>| calls.into_iter().map(|c| c[1].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(urls(discord.calls()), ["channels/111/messages?limit=1", "channels/111", "guilds/222/threads/active"]);
    let query = |after| format!("channels/{thread}/messages?after={after}&limit=100");
    let words = "--json\n给这个会话";
    discord.answer(
        "GET",
        &query(created),
        json!([msg(created + 3, "42", false, words), msg(created + 1, "42", true, "bot"), msg(created + 2, "7", false, "other")]),
    );
    discord.answer("GET", "channels/111/messages?after=100&limit=100", json!([msg(101, "42", false, "for the desk")]));
    active(created + 3);
    pull();
    assert_eq!(urls(discord.calls()), ["channels/111/messages?after=100&limit=100", "guilds/222/threads/active", &query(created)]);
    pull();
    assert_eq!(urls(discord.calls()), ["channels/111/messages?after=101&limit=100", "guilds/222/threads/active"]);
    for (id, text) in [(&worker, words), (&desk, "for the desk")] {
        let sent = session_events(&sb, id, "message.sent");
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!((&sent[0]["via"], &sent[0]["from"], &sent[0]["text"]), (&json!("Discord"), &json!("you"), &json!(text)));
        assert_eq!(sb.json(&["inbox", id, "--json"])[0]["to"], id.as_str());
    }
    assert!(discord.run(&["plugin", "run", "aca.discord.check"]).contains("reads active threads: yes"));
    discord.calls();
    // Removal while the plugin was disabled leaves a real stale mapping.
    discord.run(&["plugin", "disable", "aca.discord"]);
    discord.run(&["rm", &worker]);
    discord.run(&["plugin", "enable", "aca.discord"]);
    discord.answer("GET", &query(created + 3), json!([msg(created + 4, "42", false, "still there?")]));
    active(created + 4);
    pull();
    assert!(!discord.record(&worker).exists());
    assert_eq!(
        discord.calls().last().unwrap(),
        &json!(["POST",format!("channels/{thread}/messages"),
        {"content":"这个会话已经不在了","allowed_mentions":{"parse":[]}}])
    );
    pull();
    assert_eq!(urls(discord.calls()), ["channels/111/messages?after=101&limit=100", "guilds/222/threads/active"]);
}

/// Calling the bot in the channel puts its mention in front of your words — `<@bot>`, `<@!bot>`, or the role Discord manages
/// for it: the desk gets only the words, a message that was nothing but the mention isn't handed on, and anyone else's
/// mention stays. Who the bot is is asked once, not every minute
#[test]
fn bot_takes_its_own_mention_off_the_front_of_what_you_say() {
    let Some(sb) = Sandbox::new("discord-mention") else { return };
    let desk = desk(&sb);
    let discord = Discord::install(&sb);
    discord.setting("user", "42");
    let msg = |id: u64, content: &str| json!({"id":id.to_string(),"author":{"id":"42","bot":false},"content":content});
    let pull = || discord.run(&["plugin", "run", "aca.discord.pull"]);
    let urls = || discord.calls().into_iter().map(|c| c[1].as_str().unwrap().to_string()).collect::<Vec<_>>();
    discord.answer("GET", "channels/111", json!({"guild_id":"222"}));
    discord.answer("GET", "users/@me", json!({"id":"900","username":"aca","bot":true}));
    discord.answer(
        "GET",
        "guilds/222/roles",
        json!([
            {"id":"222","name":"@everyone"},
            {"id":"777","name":"another bot","managed":true,"tags":{"bot_id":"901"}},
            {"id":"555","name":"aca","managed":true,"tags":{"bot_id":"900"}}
        ]),
    );
    discord.answer("GET", "channels/111/messages?limit=1", json!([msg(100, "old")]));
    pull();
    // Plain words don't need to know who the bot is
    discord.answer("GET", "channels/111/messages?after=100&limit=100", json!([msg(101, "普通的一句")]));
    pull();
    assert_eq!(urls(), ["channels/111/messages?limit=1", "channels/111/messages?after=100&limit=100"]);
    discord.answer(
        "GET",
        "channels/111/messages?after=101&limit=100",
        json!([
            msg(102, "<@&555> test"),
            msg(103, "<@900>   "),
            msg(104, "<@!900> <@&555>\n两行\n话"),
            msg(105, "<@&777> 给另一个 bot"),
            msg(106, "<@43> 你看看"),
        ]),
    );
    pull();
    let sent =
        || -> Vec<String> { session_events(&sb, &desk, "message.sent").iter().map(|e| e["text"].as_str().unwrap().to_string()).collect() };
    assert_eq!(sent(), ["普通的一句", "test", "两行\n话", "<@&777> 给另一个 bot", "<@43> 你看看"]);
    assert_eq!(urls(), ["channels/111/messages?after=101&limit=100", "users/@me", "channels/111", "guilds/222/roles"]);
    discord.answer("GET", "channels/111/messages?after=106&limit=100", json!([msg(107, "<@900> 再来")]));
    pull();
    assert_eq!(urls(), ["channels/111/messages?after=106&limit=100"]);
    assert_eq!(sent().last().map(String::as_str), Some("再来"));

    // The roles can't be read this time: only the bot's own mention comes off, and it's asked again next time
    fs::remove_file(sb.home.join("state/plugins/aca.discord/self-111")).unwrap();
    let roles = discord.answer("GET", "guilds/222/roles", json!({"code":50001,"message":"Missing Access"}));
    write(&roles.with_extension("status"), "403");
    discord.answer("GET", "channels/111/messages?after=107&limit=100", json!([msg(108, "<@&555> 角色"), msg(109, "<@900> 本人")]));
    pull();
    assert_eq!(urls(), ["channels/111/messages?after=107&limit=100", "users/@me", "guilds/222/roles"]);
    discord.answer("GET", "channels/111/messages?after=109&limit=100", json!([msg(110, "<@!900> 又一次")]));
    pull();
    assert_eq!(urls(), ["channels/111/messages?after=109&limit=100", "users/@me", "guilds/222/roles"]);
    assert_eq!(sent()[6..], ["<@&555> 角色", "本人", "又一次"]);
}
