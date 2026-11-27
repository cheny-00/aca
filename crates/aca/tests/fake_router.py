"""A Claude input box and its startup MCP client, run only in the e2e sandbox."""

import json
import os
import re
import secrets
import signal
import subprocess
import sys
import threading
import time
import tty
from pathlib import Path

home = Path(os.environ["ACA_HOME"])
session = os.environ["ACA_SESSION_ID"]
shift = os.environ.get("ACA_ROUTER_SHIFT")
conversation = shift or sys.argv[sys.argv.index("--session-id") + 1]
aca = os.environ["ACA_BIN"]
manual = (home / "manual-submit").exists()
wrap_pastes = (home / "wrap-pastes").exists()
follow_bell = (home / "follow-bell").exists()


def stop(*_):
    (home / "stopping").touch()
    while (home / "hold-stop").exists():
        time.sleep(0.05)
    sys.exit(0)


def hook(event, **fields):
    data = json.dumps(dict(hook_event_name=event, session_id=conversation, **fields))
    subprocess.run([aca, "hook", "claude"], input=data.encode(), check=True)


def watch_bell(bell, last):
    (home / "watching").touch()
    while True:
        now = bell.stat().st_mtime_ns
        if now != last:
            last = now
            take = subprocess.run([aca, "inbox", "take"], input=b"{}", stderr=subprocess.PIPE)
            if take.returncode == 2:
                prompt = ("<task-notification>\n<summary>aca: 收件箱有新消息</summary>\n</task-notification>\n"
                          f"<system-reminder>\n[aca] {take.stderr.decode()}\n</system-reminder>")
                hook("UserPromptSubmit", prompt=prompt)
                (home / "bell-submit").write_text(prompt)
        time.sleep(0.02)


def connect():
    if (home / "skip-list").exists():
        return
    if (home / "delay-list").exists():
        time.sleep(2)
    config = Path(sys.argv[sys.argv.index("--mcp-config") + 1])
    server = json.loads(config.read_text())["mcpServers"]["aca"]
    reply = subprocess.check_output([server["command"], *server["args"]],
                                    input=b'{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n')
    assert any(t["name"] == "aca-router__tickets" for t in json.loads(reply)["result"]["tools"])
    (home / f"listed-{shift}").write_text(str(time.monotonic_ns()))


def draw(draft=""):
    print(f"\x1b[H\x1b[2Jready\r\n────\r\n❯ {draft}\r\n────\x1b[?2004h", flush=True)


signal.signal(signal.SIGTERM, stop)
if shift:
    with (home / "starts").open("a") as starts:
        print(shift, file=starts)
    threading.Thread(target=connect, daemon=True).start()
(home / f"args-{session}").write_bytes(b"\0".join(os.fsencode(a) for a in sys.argv[1:]) + b"\0")
tty.setraw(0)
if not manual:
    subprocess.run([aca, "inbox", "watch"], input=b"{}", stdout=subprocess.DEVNULL, check=True)
    if follow_bell:
        bell = home / "state" / "inbox" / f"{session}.bell"
        threading.Thread(target=watch_bell, args=(bell, bell.stat().st_mtime_ns), daemon=True).start()
if (home / "hold-screen").exists():
    print("loading", flush=True)
    while (home / "hold-screen").exists():
        time.sleep(0.02)

draw()

# tmux delivers LF as the terminal Return key inside the bracketed paste.
start, end = b"\x1b[200~", b"\x1b[201~"
pending = b""
while True:
    pending += os.read(0, 65536)
    if end not in pending:
        continue
    body, rest = pending.split(end, 1)
    if shift and body.startswith(start + b"[aca] "):
        stamp = home / f"received-{shift}"
        if not stamp.exists():
            stamp.write_text(str(time.monotonic_ns()))
    if not rest:
        draw(body[len(start):].decode().split("\r", 1)[0][:80])
        continue
    assert body.startswith(start) and rest.startswith(b"\r"), (body, rest)
    pending = rest[1:]
    prompt = body[len(start):].replace(b"\r", b"\n").decode()
    draw()
    opening = shift and prompt.startswith("[aca] ")
    if opening:
        (home / f"open-{shift}").write_text(prompt)
        (home / f"open-{session}").write_text(prompt)
    else:
        with (home / f"got-{session}").open("a") as received:
            print(prompt, file=received)
    if not manual and (opening or wrap_pastes or follow_bell):
        if wrap_pastes and len(prompt) > 1000:
            paste_id = secrets.token_hex(2)
            prompt = re.sub(r"<(?=/?pasted_content(?![a-z0-9_-]))", lambda _: "<\\", prompt, flags=re.ASCII | re.IGNORECASE)
            separator = "" if prompt.endswith("\n") else "\n"
            prompt = f'\n\n<pasted_content id="{paste_id}">\n{prompt}{separator}</pasted_content id="{paste_id}">\n'
            if opening:
                (home / f"submit-{shift}").write_text(prompt)
        hook("UserPromptSubmit", prompt=prompt)
        if opening:
            hook("Stop", last_assistant_message="好")
