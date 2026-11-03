#!/usr/bin/env python3
"""Stdio peer for the Pi driver tests. Speaks the 1.0.4 RPC names, no network."""

import json
import os
import select
import signal
import sys


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def respond(msg, data=None, success=True):
    body = {
        "type": "response",
        "command": msg.get("type"),
        "success": success,
    }
    if "id" in msg:
        body["id"] = msg["id"]
    if data is not None:
        body["data"] = data
    emit(body)


def log_line(stub_dir, line):
    if not stub_dir:
        return
    with open(os.path.join(stub_dir, "stdin"), "a") as handle:
        handle.write(line if line.endswith("\n") else line + "\n")


def main():
    if "--version" in sys.argv:
        print("1.0.4")
        return
    stub_dir = os.environ.get("STUB_DIR", "")
    if stub_dir:
        os.makedirs(stub_dir, exist_ok=True)
        with open(os.path.join(stub_dir, "argv"), "w") as handle:
            handle.write("\n".join(sys.argv))
        with open(os.path.join(stub_dir, "argv.json"), "w") as handle:
            json.dump(sys.argv, handle)
        with open(os.path.join(stub_dir, "env"), "w") as handle:
            for key, value in sorted(os.environ.items()):
                handle.write(f"{key}={value}\n")
        with open(os.path.join(stub_dir, "meta"), "w") as handle:
            handle.write(f"pid={os.getpid()}\npgid={os.getpgrp()}\n")
    mode = os.environ.get("STUB_MODE", "turn")
    if mode == "interrupt":

        def on_term(_signum, _frame):
            if stub_dir:
                with open(os.path.join(stub_dir, "signals"), "a") as handle:
                    handle.write("term\n")

        signal.signal(signal.SIGTERM, on_term)
    if mode == "crash":
        sys.stderr.write("drop-me\n")
        sys.stderr.write("Authorization: Bearer abc.def\n")
        sys.stderr.write("Basic xyz\n")
        sys.stderr.write("sk-SECRETVALUE\n")
        sys.stderr.write("ghp_abcdefghijklmnopqrstuvwxyz\n")
        sys.stderr.write("xox-dummy-test-token\n")
        sys.stderr.write("api_key=supersecret\n")
        sys.stderr.flush()
        raise SystemExit(1)

    session = os.environ.get("STUB_SESSION", "/tmp/pi-session.jsonl")
    # Unbuffered, so select() sees what the driver has written.
    stdin = sys.stdin.buffer.raw
    while True:
        line = stdin.readline().decode()
        if not line:
            return
        log_line(stub_dir, line)
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        kind = msg.get("type")
        if kind == "extension_ui_response":
            if stub_dir:
                with open(os.path.join(stub_dir, "response"), "w") as handle:
                    handle.write(json.dumps(msg))
            emit({"type": "agent_settled"})
            continue
        if kind == "get_state":
            respond(
                msg,
                {
                    "sessionId": "sess-1",
                    "sessionFile": session,
                    "steeringMode": "one-at-a-time",
                    "thinkingLevel": "off",
                    "isStreaming": False,
                },
            )
            continue
        if kind == "get_available_models":
            respond(
                msg,
                {
                    "models": [
                        {
                            "id": "fixture-model",
                            "name": "Fixture",
                            "provider": "local",
                        }
                    ]
                },
            )
            continue
        if kind == "get_available_thinking_levels":
            respond(msg, {"levels": ["off", "low", "high"]})
            continue
        if kind == "switch_session":
            # Real Pi answers switch_session late and cancels a prompt sent
            # before the answer. Record a driver that does not wait.
            early, _, _ = select.select([stdin], [], [], 0.3)
            if early and stub_dir:
                open(os.path.join(stub_dir, "prompt_before_switch"), "w").close()
            respond(msg, {"cancelled": False})
            continue
        if kind in ("set_thinking_level", "steer"):
            data = {"cancelled": False} if kind == "switch_session" else None
            if kind == "steer":
                data = {"disposition": "queued"}
            respond(msg, data)
            continue
        if kind == "clear_queue":
            respond(msg, {"steering": ["secret paste"], "followUp": ["nope"]})
            continue
        if kind == "abort":
            respond(msg, None)
            continue
        if kind != "prompt":
            respond(msg, None)
            continue
        if mode == "handled":
            respond(msg, {"disposition": "handled"})
            continue
        respond(msg, {"disposition": "started"})
        if mode == "unicode":
            emit(
                {
                    "type": "message_update",
                    "assistantMessageEvent": {
                        "type": "text_delta",
                        "delta": "a\u2028b\u2029c",
                        "contentIndex": 0,
                    },
                }
            )
            emit({"type": "agent_settled"})
            continue
        if mode == "question":
            emit(
                {
                    "type": "extension_ui_request",
                    "id": "q-1",
                    "method": "select",
                    "title": "Allow dangerous command?",
                    "options": ["Allow", "Block"],
                }
            )
            emit(
                {
                    "type": "extension_ui_request",
                    "id": "w-1",
                    "method": "setWidget",
                    "widgetKey": "my-ext",
                    "widgetLines": ["hidden"],
                }
            )
            continue
        if mode == "interrupt":
            emit(
                {
                    "type": "message_update",
                    "assistantMessageEvent": {
                        "type": "text_delta",
                        "delta": "partial",
                        "contentIndex": 0,
                    },
                }
            )
            continue
        if mode == "past-end":
            emit({"type": "agent_end", "messages": [], "willRetry": False})
            emit(
                {
                    "type": "message_update",
                    "assistantMessageEvent": {
                        "type": "text_delta",
                        "delta": "after-end",
                        "contentIndex": 0,
                    },
                }
            )
            emit({"type": "agent_settled"})
            continue
        emit({"type": "agent_start"})
        emit(
            {
                "type": "tool_execution_start",
                "toolCallId": "t1",
                "toolName": "bash",
                "args": {"cmd": "true"},
            }
        )
        emit(
            {
                "type": "tool_execution_end",
                "toolCallId": "t1",
                "toolName": "bash",
                "result": "ok\nrest",
                "isError": False,
            }
        )
        emit(
            {
                "type": "message_update",
                "assistantMessageEvent": {
                    "type": "text_delta",
                    "delta": "fixture-ok",
                    "contentIndex": 0,
                },
            }
        )
        emit({"type": "agent_end", "messages": [], "willRetry": False})
        emit({"type": "agent_settled"})


if __name__ == "__main__":
    main()
