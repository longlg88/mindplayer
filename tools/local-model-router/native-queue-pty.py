#!/usr/bin/env python3
"""JSONL-controlled real PTY; no shell, third-party packages, or inherited auth."""
import base64
import errno
import fcntl
import json
import os
import pty
import select
import signal
import struct
import sys
import termios
import time


def emit(message):
    print(json.dumps(message), flush=True)


def main():
    start = json.loads(sys.stdin.buffer.readline())
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(start["cwd"])
        os.execve(start["binary"], [start["binary"], *start["args"]], start["env"])
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    emit({"event": "started", "pid": pid})
    pending = b""
    query_tail = b""
    stopping = None
    try:
        while True:
            ended, status = os.waitpid(pid, os.WNOHANG)
            if ended:
                emit({"event": "exit", "code": os.waitstatus_to_exitcode(status)})
                return
            if stopping is not None and time.monotonic() - stopping > 1:
                os.kill(pid, signal.SIGKILL)
            readable, _, _ = select.select([master, sys.stdin.fileno()], [], [], 0.05)
            if master in readable:
                try:
                    output = os.read(master, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    output = b""
                if output:
                    emit({"event": "output", "data": base64.b64encode(output).decode()})
                    # Crossterm queries the terminal before entering its event loop.
                    queries = query_tail + output
                    for query, response in [
                        (b"\x1b[6n", b"\x1b[1;1R"),
                        (b"\x1b[c", b"\x1b[?1;2c"),
                        (b"\x1b[>c", b"\x1b[>0;0;0c"),
                        (b"\x1b[?u", b"\x1b[?0u"),
                        (b"\x1b]10;?\x1b\\", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
                        (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
                    ]:
                        if query in queries:
                            os.write(master, response)
                    query_tail = queries[-8:]
            if sys.stdin.fileno() in readable:
                chunk = os.read(sys.stdin.fileno(), 65536)
                if not chunk:
                    return
                pending += chunk
                while b"\n" in pending:
                    line, pending = pending.split(b"\n", 1)
                    command = json.loads(line)
                    if command["op"] == "write":
                        data = base64.b64decode(command["data"])
                        while data:
                            data = data[os.write(master, data):]
                    elif command["op"] == "stop":
                        os.kill(pid, signal.SIGTERM)
                        stopping = time.monotonic()
                    else:
                        raise ValueError("unknown bridge operation")
                    emit({"event": "ack", "id": command["id"]})
    finally:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(pid, 0)
        except ChildProcessError:
            pass
        os.close(master)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        emit({"event": "bridge-error", "type": type(error).__name__, "message": str(error)})
        sys.exit(1)
