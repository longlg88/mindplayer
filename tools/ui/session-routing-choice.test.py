#!/usr/bin/env python3
"""Real-PTY checks for the standalone Bash UI, using only Python and tmux.

Run: python3 -B tools/ui/session-routing-choice.test.py
An isolated, uniquely named tmux server owns every test PTY. No existing server,
credentials, user sessions, shell startup files, network, or packages are used.
PASS lines and the final JSON describe prototype behavior, not real Jev/CLI use.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


SCRIPT = Path(__file__).with_name("session-routing-choice.command")
CHOICE = "모델 판단 방식"
JUDGING = "Jev 판단 중 (시연)"
REQUESTED = "요청 수락 (시연)"
RUNNING = "실행 중 (시연)"
DONE = "완료 (시연)"
FOOTER = ("UI 시연", "실제 API 호출 없음")
TIMEOUT = 8.0

# The parent shell also receives terminal SIGINT. Its trap keeps it alive long
# enough to measure the child's restoration and preserve the child's exit code.
WRAPPER = r'''
before=$(stty -g) || exit 91
trap ':' INT
marker=$1
shift
"$@"
result=$?
after=$(stty -g) || exit 92
restored=no
if [ "$before" = "$after" ]; then restored=yes; fi
printf '\n%s=%s:%s\n' "$marker" "$result" "$restored"
exit "$result"
'''


def require(condition, message):
    if not condition:
        raise AssertionError(message)


class Terminal:
    def __init__(self, suite, bash, args=(), width=120, height=30):
        self.suite = suite
        self.marker = "__MPR_" + uuid.uuid4().hex[:12]
        command = shlex.join([
            "/bin/sh", "-c", WRAPPER, "ui-test", self.marker,
            bash, "--noprofile", "--norc", str(suite.script), *args,
        ])
        self.pane = suite.tmux(
            "new-window", "-d", "-P", "-F", "#{pane_id}",
            "-t", "ui-tests:", "-c", str(suite.work), command,
        ).strip()
        self.finished = False
        self.resize(width, height)

    def __enter__(self):
        return self

    def __exit__(self, error_type, error, traceback):
        try:
            if error_type is None and not self.finished:
                self.finish("C-c", 130)
        finally:
            # This target was created on this runner's unique private server.
            self.suite.tmux("kill-window", "-t", self.pane, check=False)

    def screen(self):
        return self.suite.tmux("capture-pane", "-p", "-t", self.pane)

    def wait(self, predicate, description, timeout=TIMEOUT):
        end = time.monotonic() + timeout
        last = ""
        while time.monotonic() < end:
            last = self.screen()
            if predicate(last):
                return last
            if self.marker + "=" in last:
                raise AssertionError("UI exited while waiting for " + description + "\n" + last)
            time.sleep(0.04)
        raise AssertionError("Timed out waiting for " + description + "\n" + last)

    def expect(self, *markers):
        text = self.wait(lambda screen: all(marker in screen for marker in markers), repr(markers))
        require(all(marker in text for marker in FOOTER), "Missing prototype footer\n" + text)
        return text

    def keys(self, *keys):
        self.suite.tmux("send-keys", "-t", self.pane, *keys)

    def literal(self, text):
        # An argv argument, not shell interpolation or tmux key-name lookup.
        self.suite.tmux("send-keys", "-l", "-t", self.pane, "--", text)

    def draft(self, text):
        wanted = ("> " + text).strip()
        return self.wait(lambda screen: wanted in [line.strip() for line in screen.splitlines()], "draft " + repr(text))

    def submit(self, text, state):
        self.literal(text)
        self.draft(text)
        self.keys("Enter")
        return self.expect("입력: " + text, state)

    def listing(self):
        return self.wait(lambda screen: "세션 목록" in screen and CHOICE not in screen, "session list")

    def choice(self):
        return self.expect(CHOICE, "Jev 사용 안 함")

    def session(self):
        return self.wait(lambda screen: "Ctrl-X 목록" in screen and CHOICE not in screen, "session input")

    def resize(self, width, height):
        self.suite.tmux("resize-window", "-t", self.pane, "-x", str(width), "-y", str(height))

    def finish(self, key, expected):
        self.keys(key)
        text = self.wait(lambda screen: self.marker + "=" in screen, "exit and terminal restoration")
        match = re.search(re.escape(self.marker) + r"=(\d+):(yes|no)", text)
        require(match is not None, "Missing exit measurement\n" + text)
        require(int(match[1]) == expected, "Unexpected exit code\n" + text)
        require(match[2] == "yes", "stty settings were not restored\n" + text)
        self.finished = True


class Suite:
    def __init__(self, script, root, tmux):
        self.script = script
        self.root = Path(root)
        self.work = self.root / "work"
        self.work.mkdir()
        self.socket = "mpr-ui-" + uuid.uuid4().hex[:12]
        self.binary = tmux
        # Whitelist rather than inheriting auth, BASH_ENV, ENV, TMUX or profiles.
        self.env = {
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin",
            "LANG": "en_US.UTF-8", "LC_ALL": "en_US.UTF-8",
            "TERM": "xterm-256color", "TMUX_TMPDIR": str(self.root),
            "TMPDIR": str(self.root),
        }

    def run(self, command):
        return subprocess.run(command, cwd=self.work, env=self.env, stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=TIMEOUT,
                              text=True, encoding="utf-8", errors="strict")

    def tmux(self, *args, check=True):
        result = self.run([self.binary, "-u", "-L", self.socket, "-f", "/dev/null", *args])
        if check and result.returncode:
            raise AssertionError("tmux " + repr(args) + ": " + result.stderr)
        return result.stdout

    def start(self):
        # An inert keeper window prevents automatic server shutdown between cases.
        self.tmux("new-session", "-d", "-s", "ui-tests", "-x", "120", "-y", "30", "/bin/cat")
        self.tmux("set-option", "-g", "default-shell", "/bin/sh")
        self.tmux("set-option", "-g", "status", "off")
        self.tmux("set-option", "-g", "remain-on-exit", "on")

    def stop(self):
        self.tmux("kill-server", check=False)
        # tmux may leave its socket inode after a successful shutdown. Probe the
        # explicitly named server; TemporaryDirectory removes the stale inode.
        probe = self.run([self.binary, "-u", "-L", self.socket, "-f", "/dev/null", "list-sessions"])
        return probe.returncode != 0

    def terminal(self, bash, *args, **size):
        return Terminal(self, bash, args, **size)


def non_tty(suite, bash):
    result = suite.run([bash, "--noprofile", "--norc", "-n", str(suite.script)])
    require(result.returncode == 0, "Bash syntax: " + result.stderr)
    result = suite.run([bash, "--noprofile", "--norc", str(suite.script), "--help"])
    require(result.returncode == 0, "--help requires a TTY")
    require(all(value in result.stdout for value in ("--entry", "--scenario", "Ctrl-G")), "Incomplete help")
    result = suite.run([bash, "--noprofile", "--norc", str(suite.script)])
    require(result.returncode != 0 and "TTY" in result.stderr, "Non-TTY launch must refuse")
    for args in (("--entry", "bad"), ("--scenario", "bad"), ("--entry",), ("--unknown",)):
        result = suite.run([bash, "--noprofile", "--norc", str(suite.script), *args])
        require(result.returncode != 0, "Invalid options accepted: " + repr(args))


def navigation(suite, bash):
    with suite.terminal(bash) as term:
        original = term.listing()
        selected = lambda screen: next((line.strip() for line in screen.splitlines() if line.strip().startswith("> codex")), "")
        first = selected(original)
        require(bool(first), "No selected list row")
        term.keys("Down")
        second = term.wait(lambda screen: selected(screen) != first, "Down moves selection")
        require(bool(selected(second)), "Down lost selection")
        term.keys("Up")
        term.wait(lambda screen: selected(screen) == first, "Up restores selection")
        term.keys("j")
        term.wait(lambda screen: selected(screen) == selected(second), "j moves selection")
        term.keys("k")
        term.wait(lambda screen: selected(screen) == first, "k restores selection")
        term.keys("Enter")
        term.choice()
        term.keys("Escape")
        term.listing()  # Bash 3.2 may need a whole second to disambiguate ESC.
        term.keys("n")
        term.expect(CHOICE, "새 세션")
        term.keys("Escape")
        term.listing()
        term.finish("q", 0)


def native(suite, bash):
    for entry in ("existing", "new"):
        with suite.terminal(bash, "--entry", entry) as term:
            term.choice()
            term.keys("Enter")
            term.expect("Jev 미사용", "프롬프트를 입력하세요")
            screen = term.submit("한글 프롬프트를 실행해 주세요", RUNNING)
            require(JUDGING not in screen, "Native prompt started Jev judgment")
            term.keys("Tab")
            term.expect(DONE)
            screen = term.submit("다음 한글 요청", RUNNING)
            require(JUDGING not in screen, "Native follow-up started Jev judgment")
            term.keys("Tab")
            term.expect(DONE)


def jev(suite, bash):
    with suite.terminal(bash, "--entry", "existing") as term:
        term.choice()
        term.keys("Down", "Enter")
        term.expect(CHOICE, "열기 차단", "동의")
        term.keys("c")
        term.expect("c [x]")
        term.keys("c")
        term.expect("c [ ]")
        term.keys("Enter")
        term.expect(CHOICE, "열기 차단")
        term.keys("c", "Enter")
        term.expect("Jev 사용", "프롬프트를 입력하세요")
        term.submit("첫 번째 판단 요청", JUDGING)
        for state in (REQUESTED, RUNNING, DONE):
            term.keys("Tab")
            term.expect(state)
        term.submit("두 번째도 새 판단", JUDGING)
        term.keys("Tab")
        term.expect(REQUESTED)


def unavailable(suite, bash):
    reasons = {"missing": "키 없음", "denied": "외부 전송 금지", "unsupported": "미지원"}
    for entry in ("existing", "new"):
        for scenario, reason in reasons.items():
            with suite.terminal(bash, "--entry", entry, "--scenario", scenario) as term:
                term.expect(CHOICE, reason)
                term.keys("Down", "c", "Enter")
                term.expect(CHOICE, "열기 차단", reason)
                term.keys("Up", "Enter")
                term.expect("Jev 미사용", "프롬프트를 입력하세요")
                term.submit("키 없이 기존 방식", RUNNING)


def queue_and_reattach(suite, bash):
    with suite.terminal(bash, "--entry", "new") as term:
        term.choice()
        term.keys("Down", "c", "Enter")
        term.session()
        term.submit("먼저 실행하는 요청", JUDGING)
        term.literal("개별 대기 첫째")
        term.draft("개별 대기 첫째")
        term.keys("Enter")
        term.expect("대기 입력: 1", "입력: 먼저 실행하는 요청")
        term.literal("개별 대기 둘째")
        term.draft("개별 대기 둘째")
        term.keys("Enter")
        term.expect("대기 입력: 2", "입력: 먼저 실행하는 요청")
        term.literal("보존할 초안")
        term.draft("보존할 초안")
        term.keys("C-x")
        term.listing()
        term.keys("x")
        term.expect("종료 차단")
        term.keys("Enter")
        term.expect(JUDGING, "대기 입력: 2", "입력: 먼저 실행하는 요청")
        require(CHOICE not in term.screen(), "Reattach reopened mode selection")
        term.draft("보존할 초안")
        for state in (REQUESTED, RUNNING, DONE):
            term.keys("Tab")
            term.expect(state)
        term.keys("C-x")
        term.listing()
        term.keys("x")
        term.expect("종료 차단")  # Completed work still has two queued prompts.
        term.keys("Enter")
        term.expect(DONE, "대기 입력: 2")
        for prompt, remaining in (("개별 대기 첫째", 1), ("개별 대기 둘째", 0)):
            term.keys("Tab")
            term.expect(JUDGING, "입력: " + prompt, "대기 입력: " + str(remaining))
            term.draft("보존할 초안")
            for state in (REQUESTED, RUNNING, DONE):
                term.keys("Tab")
                term.expect(state)
        term.keys("C-x")
        term.listing()
        term.keys("x")
        term.expect("안전 종료")
        term.keys("Enter")
        term.choice()


def close_idle_and_busy(suite, bash):
    with suite.terminal(bash, "--entry", "existing") as term:
        term.choice()
        term.keys("Enter")
        term.session()
        term.keys("C-x")
        term.listing()
        term.keys("x")
        term.expect("안전 종료")
        term.keys("Enter")
        term.choice()
        term.keys("Enter")
        term.session()
        term.submit("실행 중 종료 거절", RUNNING)
        term.keys("C-x")
        term.listing()
        term.keys("x")
        term.expect("종료 차단")
        term.keys("Enter")
        term.expect(RUNNING, "입력: 실행 중 종료 거절")
        term.keys("Tab")
        term.expect(DONE)
        term.keys("C-x")
        term.listing()
        term.keys("x")
        term.expect("안전 종료")
        term.keys("Enter")
        term.choice()


def literal_and_edit(suite, bash):
    with suite.terminal(bash, "--entry", "existing", width=160) as term:
        term.choice()
        term.keys("Enter")
        term.session()
        term.literal("한글끝")
        term.draft("한글끝")
        term.keys("BSpace")
        term.draft("한글")
        term.keys("BSpace", "BSpace", "BSpace")
        term.draft("")
        payload = '한글 $(touch marker-dollar) `touch marker-backtick`; %s "$HOME" \\ 끝'
        term.submit(payload, RUNNING)
        require(not (suite.work / "marker-dollar").exists(), "Dollar substitution was evaluated")
        require(not (suite.work / "marker-backtick").exists(), "Backtick substitution was evaluated")
        term.keys("Tab")
        term.expect(DONE)
        term.literal("qnt")
        term.draft("qnt")
        term.keys("C-g")
        term.expect("Trace (", "실제 실행 증거 아님")
        term.keys("C-g")
        term.expect(DONE, "입력: " + payload)
        term.draft("qnt")


def resize(suite, bash):
    with suite.terminal(bash, "--entry", "existing") as term:
        term.choice()
        term.resize(42, 12)
        term.wait(lambda screen: "넓혀주세요" in screen, "small-terminal fallback")
        term.resize(120, 30)
        term.choice()
        term.keys("Enter")
        term.session()
        term.literal("한글 초안 유지")
        term.draft("한글 초안 유지")
        for width, height in ((80, 24), (42, 12), (120, 30)):
            term.resize(width, height)
            if width < 48 or height < 19:
                term.wait(lambda screen: "넓혀주세요" in screen, "small session fallback")
            else:
                term.expect("Ctrl-X 목록")
                term.draft("한글 초안 유지")
        term.keys("Enter")
        term.expect("입력: 한글 초안 유지", RUNNING)
        term.keys("C-x")
        term.listing()
        term.finish("q", 0)


def interrupt(suite, bash):
    for entry in ("list", "existing", "new"):
        with suite.terminal(bash, "--entry", entry) as term:
            if entry == "list":
                term.listing()
            else:
                term.choice()
                if entry == "new":
                    term.keys("Enter")
                    term.session()
                    term.submit("한글 실행 중 인터럽트", RUNNING)
            term.finish("C-c", 130)


CASES = (non_tty, navigation, native, jev, unavailable, queue_and_reattach,
         close_idle_and_busy, literal_and_edit, resize, interrupt)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--script", type=Path, default=SCRIPT)
    parser.add_argument("--bash", action="append", help="Bash executable; repeat for multiple versions")
    parser.add_argument("--case", action="append", choices=[case.__name__ for case in CASES])
    options = parser.parse_args()
    script = options.script.resolve()
    require(script.is_file(), "Script not available yet: " + str(script))
    binary = shutil.which("tmux")
    require(binary is not None, "Installed tmux is required; do not install packages")
    shells = options.bash or [path for path in ("/bin/bash", "/opt/homebrew/bin/bash") if Path(path).is_file()]
    require(bool(shells), "No Bash executable available")
    digest = hashlib.sha256(script.read_bytes()).hexdigest()
    report = {
        "scope": "standalone terminal UI prototype only; not Rust, real CLI, Jev inference or installation",
        "script": str(script), "scriptSha256": digest, "result": "passed",
        "bashVersions": {}, "checks": [], "failures": [],
        "limitations": ["No real provider/API/model calls", "No user session or credential access",
                        "No guarantee for every terminal emulator or malformed UTF-8 input"],
    }
    with tempfile.TemporaryDirectory(prefix="mpr-ui-", dir="/tmp") as root:
        suite = Suite(script, root, binary)
        try:
            suite.start()
            for bash in shells:
                version = suite.run([bash, "--noprofile", "--norc", "--version"])
                require(version.returncode == 0, "Cannot execute " + bash)
                report["bashVersions"][bash] = version.stdout.splitlines()[0]
                for case in CASES:
                    if options.case and case.__name__ not in options.case:
                        continue
                    name = bash + ": " + case.__name__
                    try:
                        case(suite, bash)
                        require(not list(suite.work.iterdir()), "Prototype wrote files to its working directory")
                        report["checks"].append(name)
                        print("PASS " + name, flush=True)
                    except Exception as error:
                        report["failures"].append({"check": name, "error": str(error)})
                        print("FAIL " + name + "\n" + str(error), file=sys.stderr, flush=True)
        finally:
            report["dedicatedTmuxServerCleaned"] = suite.stop()
    if hashlib.sha256(script.read_bytes()).hexdigest() != digest:
        report["failures"].append({"check": "source stability", "error": "Script changed during this run; rerun against the final source"})
    if report["failures"] or not report["dedicatedTmuxServerCleaned"]:
        report["result"] = "failed"
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if report["result"] == "passed" else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (AssertionError, OSError, subprocess.SubprocessError) as error:
        print("FAIL " + str(error), file=sys.stderr)
        sys.exit(1)
