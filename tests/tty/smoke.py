#!/usr/bin/env python3
"""Drives the real sparsh binary through a pseudo-terminal and checks the
completion features that unit tests cannot see (the editor decides what text
the completer receives). Usage: SPARSH_BIN=target/release/sparsh python3 tests/tty/smoke.py"""
import fcntl, os, pty, re, select, struct, sys, tempfile, termios, time

BIN = os.environ.get("SPARSH_BIN", "sparsh")


def run(steps, cwd, wait=0.8):
    env = dict(os.environ, HOME=tempfile.mkdtemp(), TERM="xterm-256color")
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.execvpe(BIN, ["sparsh"], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))

    def drain(seconds):
        out, end = b"", time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([fd], [], [], 0.1)
            if not ready:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            out += data
            for _ in range(data.count(b"\x1b[6n")):  # cursor position query
                os.write(fd, b"\x1b[5;10R")
        return out

    outs = [drain(2.0)]
    for step in steps:
        os.write(fd, step.encode())
        outs.append(drain(wait))
    try:
        os.kill(pid, 9)
    except OSError:
        pass
    return outs


def clean(raw):
    text = raw.decode("utf8", "replace")
    text = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", text)
    return text.replace("\r", "")


def check(label, steps, cwd, expect, forbid=()):
    screen = clean(run(steps, cwd)[-1])
    ok = all(e in screen for e in expect) and not any(f in screen for f in forbid)
    print(("ok   " if ok else "FAIL ") + label)
    if not ok:
        print(screen[-600:])
    return ok


def main():
    d = tempfile.mkdtemp()
    open(d + "/a.txt", "w").write("x")
    os.mkdir(d + "/dir1")
    open(d + "/lib.spar", "w").write(
        "export var port: int = 80;\nfunction greet(name: str) -> str { return name; };\n"
    )
    left = "\x1b[D"
    tail = '} from "lib.spar";'
    results = [
        check("cat offers files only", ["cat ", "\t"], d, ["a.txt", "lib.spar"], ["dir1"]),
        check("cd offers directories only", ["cd ", "\t"], d, ["cd dir1/"], ["a.txt"]),
        check("import names from a file", ["import { ", tail, left * len(tail), "\t"], d, ["greet", "port"]),
        check("import names from a package", ['import pkg { ', '} from "std/fs";', left * len('} from "std/fs";'), "\t"], d, ["readText", "exists"]),
        check("member completion", ['struct P { name: str = ""; port: int = 0; };\r', "var p: P = P()\r", "p.", "\t"], d, ["name", "port"]),
        check("scope completion", ["var count: int = 1\r", "var x = co", "\t"], d, ["count"]),
        check("signature hint", ["fn build(profile: str, release: bool = false) -> str { return profile; };\r", "build(profile: "], d, ["release: bool = false"]),
        check("mixed line runs", ["echo hi; var a: int = 2; a + 1\r"], d, ["hi", "3"]),
    ]
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
