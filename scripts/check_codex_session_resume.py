#!/usr/bin/env python3
"""Opt-in live Codex validation; never submit a turn or print transcript/auth data.

Protocol references: https://developers.openai.com/codex/app-server
Cross-check request fields against `codex app-server generate-json-schema`.
"""

import argparse
import json
import os
import queue
import subprocess
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--home", required=True)
    parser.add_argument("--thread", action="append", required=True)
    parser.add_argument("--resume", action="store_true", help="load threads, but never start a turn")
    parser.add_argument("--timeout", type=float, default=60)
    args = parser.parse_args()
    env = os.environ.copy()
    env["CODEX_HOME"] = os.path.abspath(args.home)
    for key in ("CODEX_ACCESS_TOKEN", "CODEX_API_KEY", "OPENAI_API_KEY"):
        env.pop(key, None)
    process = subprocess.Popen(
        ["codex", "app-server", "--listen", "stdio://"],
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    messages = queue.Queue()

    def read_messages():
        try:
            for line in process.stdout:
                messages.put(json.loads(line))
        finally:
            messages.put(None)

    threading.Thread(target=read_messages, daemon=True).start()
    request_id = 0

    def send(message):
        process.stdin.write(json.dumps(message) + "\n")
        process.stdin.flush()

    def request(method, params):
        nonlocal request_id
        request_id += 1
        send({"id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + args.timeout
        while True:
            try:
                message = messages.get(timeout=max(0, deadline - time.monotonic()))
            except queue.Empty as error:
                raise RuntimeError(f"{method}: timed out") from error
            if message is None:
                raise RuntimeError(f"{method}: app-server exited")
            if message.get("id") != request_id:
                continue
            if "error" in message:
                error = message["error"]
                detail = error.get("message", "")
                reason = "RPC error"
                if "already has an active writer" in detail:
                    reason = "existing active writer (left untouched)"
                elif "no rollout found" in detail:
                    reason = "no rollout found"
                raise RuntimeError(f"{method}: {error.get('code')} {reason}")
            return message["result"]

    try:
        request("initialize", {"clientInfo": {"name": "mindplayer_validation", "version": "0.55.2"}})
        send({"method": "initialized", "params": {}})
        for thread_id in args.thread:
            stored = request("thread/read", {"threadId": thread_id, "includeTurns": False})["thread"]
            if stored["id"] != thread_id:
                raise RuntimeError("thread/read returned a different thread")
            result = {"thread": thread_id, "read": "PASS"}
            print(json.dumps(result), flush=True)
            if args.resume:
                resumed = request("thread/resume", {"threadId": thread_id, "excludeTurns": True})["thread"]
                if resumed["id"] != thread_id:
                    raise RuntimeError("thread/resume returned a different thread")
                request("thread/unsubscribe", {"threadId": thread_id})
                print(json.dumps({"thread": thread_id, "resume": "PASS"}), flush=True)
        print("PASS: no turn/start sent; transcript and credentials not printed", flush=True)
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        raise SystemExit(str(error)) from None
