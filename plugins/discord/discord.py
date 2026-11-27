# aca.discord, bot mode: post.sh and pull.sh hand over here once token and channel are set.
#   discord.py post [test]   an event (or a test message): to the main channel or the session's thread
#   discord.py [pull]        the timer: what you said in the main channel goes to the desk, in a thread to its session
#   discord.py check         what the bot can see, for setting it up
# Standard library only, and nothing newer than Python 3.8 has (macOS ships 3.9, Ubuntu 22.04 3.10).
import fcntl
import functools
import hashlib
import json
import os
import re
import subprocess
import sys
from contextlib import contextmanager
from pathlib import Path

if sys.version_info < (3, 8):
    print("aca.discord: needs Python 3.8 or later")
    sys.exit(1)

env = os.environ
token = env.get("ACA_SETTING_TOKEN", "")
channel = env.get("ACA_SETTING_CHANNEL", "")
mention = env.get("ACA_SETTING_MENTION", "")
user = env.get("ACA_SETTING_USER") or mention
aca = env["ACA_BIN"]
state = Path(env["ACA_PLUGIN_STATE_DIR"])
records = state / ("threads-" + channel)


class UnknownChannel(RuntimeError):
    pass


class ArchivedThread(RuntimeError):
    pass


class Unreachable(RuntimeError):
    pass


def snowflake(value):
    if not isinstance(value, str) or not re.fullmatch("[0-9]+", value):
        raise ValueError("expected a Discord id (digits only)")
    return value


def api(method, path, body=None):
    args = ["curl", "-sS", "--max-time", "20", "-w", "%{stderr}\n%{http_code}",
            "-H", "Authorization: Bot " + token, "-X", method]
    if body is not None:
        args += ["-H", "Content-Type: application/json", "-d", json.dumps(body)]
    result = subprocess.run(args + ["https://discord.com/api/v10/" + path], capture_output=True, text=True)
    status = result.stderr.strip().rsplit("\n", 1)[-1]
    try:
        data = json.loads(result.stdout) if result.stdout.strip() else {}
    except ValueError:
        data = {"message": "unexpected answer"}
    if status == "404" or isinstance(data, dict) and data.get("code") == 10003:
        raise UnknownChannel("unknown Discord channel")
    if isinstance(data, dict) and data.get("code") == 50083:
        raise ArchivedThread("thread is archived")
    if result.returncode:
        raise Unreachable("Discord can't be reached")
    if (status and not status.startswith("2")) or isinstance(data, dict) and ("message" in data or "code" in data):
        reason = data.get("message") if isinstance(data, dict) else None
        raise RuntimeError(reason or f"Discord HTTP {status}")
    return data


def read(path):
    try:
        return path.read_text().strip()
    except FileNotFoundError:
        return ""


def save(path, value):
    tmp = path.with_name(f".{path.name}.{os.getpid()}")
    tmp.write_text(value)
    tmp.replace(path)


@contextmanager
def lock(name):
    locks = state / "locks"
    locks.mkdir(parents=True, exist_ok=True)
    with (locks / name).open("a") as file:
        fcntl.flock(file, fcntl.LOCK_EX)
        yield


def seen(thread):
    return state / ("seen-" + snowflake(thread))


def forget(record, thread):
    record.unlink(missing_ok=True)
    seen(thread).unlink(missing_ok=True)


def thread_name(title, session):
    return (title if title.strip() else session)[:100]


def create(record, name):
    data = api("POST", f"channels/{channel}/threads", {"name": name, "type": 11, "auto_archive_duration": 1440})
    thread = snowflake(data["id"])
    # The new thread's own snowflake marks its creation, even when it has no messages yet.
    save(seen(thread), thread)
    records.mkdir(exist_ok=True)
    save(record, thread)
    return thread


def message(target, content, ping=""):
    allowed = {"parse": []}
    if ping:
        allowed["users"] = [ping]
    api("POST", f"channels/{target}/messages", {"content": content, "allowed_mentions": allowed})


def update_thread(thread, patch):
    path = "channels/" + thread
    try:
        api("PATCH", path, patch)
    except ArchivedThread:
        if "name" in patch:
            # Discord requires an archived thread to reopen for a name change.
            api("PATCH", path, {**patch, "archived": False})
            api("PATCH", path, {"archived": True})


def is_desk(session, group):
    # aca marks the scratch group, whose router `aca msg desk` reaches; like aca, the router counts only while still in it.
    groups = json.loads(subprocess.check_output([aca, "group", "ls", "--json"]))
    return any(g.get("scratch") is True and g.get("router") == session and g.get("path") == group for g in groups)


def post():
    if sys.argv[2:] == ["test"]:
        message(channel, "**aca**\nDiscord works")
        print("posted")
        return
    event = env["ACA_PLUGIN_EVENT"]
    data = json.loads(env["ACA_PLUGIN_EVENT_JSON"])
    session = env["ACA_SESSION_ID"]
    if not re.fullmatch("[a-zA-Z0-9_-]+", session):
        raise ValueError("expected a session id")
    record = records / session
    with lock("session-" + session):
        thread = read(record)
        if event.startswith("session."):
            if not thread:
                return
            patch = ({"name": thread_name(data["title"], session)} if event in ("session.renamed", "session.titled")
                     else {"archived": True})
            try:
                update_thread(thread, patch)
            except UnknownChannel:
                forget(record, thread)
                return
            if event == "session.removed":
                forget(record, thread)
            return

        row = subprocess.run([aca, "show", session, "--json"], capture_output=True, text=True)
        if row.returncode:
            return
        row = json.loads(row.stdout)
        reply = data.get("via") == "Discord"
        if not reply and (row.get("attached") or row.get("parent") and env.get("ACA_SETTING_WORKERS") != "true"):
            return
        main = reply and is_desk(session, row["group"])
        field, mark = {"turn.done": ("last", "✓"), "agent.waiting": ("ask", "?"), "turn.failed": ("error", "✕")}[event]
        ping = mention if event == "agent.waiting" else ""
        prefix = "" if main else mark + " "
        if ping:
            prefix += f"<@{ping}> "
        text = prefix + (data.get(field) or "")[:4000]
        if not text:
            return
        name = thread_name(row["title"], session)
        target = channel if main else thread or create(record, name)
        # If the thread disappears between parts, repeat the whole reply in its replacement.
        for attempt in range(2):
            try:
                for i in range(0, len(text), 2000):
                    message(target, text[i:i + 2000], ping if i == 0 else "")
                return
            except UnknownChannel:
                if main or attempt:
                    raise
                forget(record, target)
                target = create(record, name)


def guild():
    cache = state / ("guild-" + channel)
    found = read(cache)
    if not found:
        found = snowflake(api("GET", "channels/" + channel)["guild_id"])
        save(cache, found)
    return found


def active_threads():
    return api("GET", f"guilds/{guild()}/threads/active")["threads"]


@functools.lru_cache(maxsize=None)
def bot_self():
    # Who the bot is: its user id, and the roles Discord manages for it in this server (picking the bot from the @ list can
    # insert its role's mention instead). Asked once per token and kept in the state directory, not every minute.
    cache = state / ("self-" + channel)
    key = hashlib.sha256(token.encode()).hexdigest()[:16]
    try:
        known = json.loads(read(cache) or "{}")
    except ValueError:
        known = {}
    if isinstance(known, dict) and known.get("key") == key and isinstance(known.get("roles"), list) and known.get("bot"):
        return known["bot"], known["roles"]
    bot = snowflake(api("GET", "users/@me")["id"])
    try:
        roles = api("GET", f"guilds/{guild()}/roles")
    except Unreachable:
        raise
    except RuntimeError:
        # Not allowed to list the roles, or Discord failed this time: go by the bot's own mention for this run, ask again
        # on the next.
        return bot, []
    mine = [r["id"] for r in roles if isinstance(r, dict) and (r.get("tags") or {}).get("bot_id") == bot]
    save(cache, json.dumps({"key": key, "bot": bot, "roles": mine}))
    return bot, mine


mention_at_start = re.compile(r"\s*<@([!&]?)([0-9]+)>\s*")


def unaddressed(words):
    # Mentions of the bot in front are how you call it in the channel, not part of what you said; anyone else's stay.
    if not mention_at_start.match(words):
        return words
    bot, roles = bot_self()
    while True:
        found = mention_at_start.match(words)
        if not found:
            return words
        kind, target = found.groups()
        if not (target in roles if kind == "&" else target == bot):
            return words
        words = words[found.end():]


def messages(target, query):
    result = api("GET", f"channels/{target}/messages?{query}")
    if not isinstance(result, list):
        raise ValueError("expected a list of messages")
    return result


def check():
    latest = messages(channel, "limit=5")
    print("reads the channel: yes")
    mine = [m for m in latest if m["author"]["id"] == user]
    if not mine:
        print("your messages: none among the latest 5 (say something there, then check again)")
    elif any(m.get("content") for m in mine):
        print("sees what you wrote: yes")
    else:
        print("sees what you wrote: no (turn on Message Content Intent for the bot)")
    try:
        active_threads()
    except (KeyError, TypeError, ValueError, RuntimeError) as err:
        raise RuntimeError(f"reads active threads: no ({err})") from err
    print("reads active threads: yes")


def receive(target, session, last):
    cursor = seen(target)
    if not last:
        latest = messages(target, "limit=1")
        save(cursor, snowflake(latest[0]["id"]) if latest else "0")
        return
    batch = messages(target, f"after={last}&limit=100")
    no_desk = False
    for msg in sorted(batch, key=lambda m: int(snowflake(m["id"]))):
        author, words = msg["author"], msg.get("content", "")
        if author["id"] == user and not author.get("bot") and not no_desk:
            words = unaddressed(words)
        else:
            words = ""
        # Nothing left once the mention is taken off: nothing to hand on.
        if words.strip():
            sent = subprocess.run([aca, "msg", session, "--via", "Discord"], input=words, text=True, capture_output=True)
            if session == "desk" and sent.returncode == 3:
                no_desk = True
            elif sent.returncode:
                error = sent.stderr.strip()
                if session != "desk" and error in (f"no session {session}", f"没有会话 {session}"):
                    words = "这个会话已经不在了" if env.get("ACA_LANG") == "zh" else "This session no longer exists."
                    message(target, words)
                    forget(records / session, target)
                    return
                raise RuntimeError(error or "a message wasn't taken in, trying again next time")
        save(cursor, msg["id"])


def pull():
    with lock("pull-" + channel):
        receive(channel, "desk", read(seen(channel)))
        mappings = list(records.glob("[!.]*"))
        if not mappings:
            return
        latest = {t["id"]: int(t.get("last_message_id") or 0) for t in active_threads() if t.get("parent_id", channel) == channel}
        for record in mappings:
            with lock("session-" + record.name):
                thread = read(record)
                if thread in latest:
                    last = read(seen(thread))
                    if latest[thread] > int(last or 0):
                        try:
                            receive(thread, record.name, last)
                        except UnknownChannel:
                            # A deletion after the listing: the next outgoing event replaces this thread.
                            continue


def main():
    action = sys.argv[1] if len(sys.argv) > 1 else "pull"
    snowflake(channel)
    if action != "post":
        snowflake(user)
    state.mkdir(parents=True, exist_ok=True)
    try:
        {"post": post, "pull": pull, "check": check}[action]()
    except Unreachable:
        # A timer retries its saved position on the next tick.
        if action != "pull":
            raise


try:
    main()
except (OSError, ValueError, KeyError, TypeError, RuntimeError, subprocess.CalledProcessError) as error:
    print(f"aca.discord: {error}")
    sys.exit(1)
