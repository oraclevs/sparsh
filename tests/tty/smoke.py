#!/usr/bin/env python3
"""Drives the real sparsh binary through a pseudo-terminal and checks the
completion features that unit tests cannot see (the editor decides what text
the completer receives). Usage: SPARSH_BIN=target/release/sparsh python3 tests/tty/smoke.py"""
import fcntl, os, pty, select, struct, sys, tempfile, termios, time

BIN = os.environ.get("SPARSH_BIN", "sparsh")


class Screen:
    """A small terminal emulator: enough of xterm for reedline's output
    (cursor moves, erase, save/restore, wrapping, scrolling). It also answers
    cursor-position queries with the emulated cursor, so reedline's absolute
    repaints land where a real terminal would put them."""

    def __init__(self, rows, cols, reply):
        self.rows, self.cols, self.reply = rows, cols, reply
        self.grid = [[" "] * cols for _ in range(rows)]
        self.r = self.c = 0
        self.saved = (0, 0)
        self.pending_wrap = False
        self.buf = ""
        self.state = None  # None | "esc" | "csi" | "osc"

    def lines(self):
        return ["".join(row).rstrip() for row in self.grid]

    def text(self):
        return "\n".join(self.lines())

    def _newline(self):
        if self.r == self.rows - 1:
            self.grid.pop(0)
            self.grid.append([" "] * self.cols)
        else:
            self.r += 1

    def _put(self, ch):
        if self.pending_wrap:
            self.c = 0
            self._newline()
            self.pending_wrap = False
        self.grid[self.r][self.c] = ch
        if self.c == self.cols - 1:
            self.pending_wrap = True
        else:
            self.c += 1

    def feed(self, data):
        for ch in data.decode("utf8", "replace"):
            if self.state == "esc":
                if ch == "[":
                    self.state, self.buf = "csi", ""
                elif ch == "]":
                    self.state, self.buf = "osc", ""
                else:
                    if ch == "7":
                        self.saved = (self.r, self.c)
                    elif ch == "8":
                        self.r, self.c = self.saved
                        self.pending_wrap = False
                    self.state = None
            elif self.state == "csi":
                if ch.isalpha() or ch in "@`~":
                    self._csi(self.buf, ch)
                    self.state = None
                else:
                    self.buf += ch
            elif self.state == "osc":
                if ch in "\x07\x1b":
                    self.state = None if ch == "\x07" else "esc"
            elif ch == "\x1b":
                self.state = "esc"
            elif ch == "\r":
                self.c, self.pending_wrap = 0, False
            elif ch == "\n":
                self._newline()
                self.pending_wrap = False
            elif ch == "\b":
                self.c, self.pending_wrap = max(0, self.c - 1), False
            elif ch >= " ":
                self._put(ch)

    def _csi(self, params, final):
        nums = [int(x) if x.isdigit() else 0 for x in params.lstrip("?").split(";")] if params else []
        n = nums[0] if nums and nums[0] else 1
        self.pending_wrap = False
        if params.startswith("?"):
            return
        if final == "A":
            self.r = max(0, self.r - n)
        elif final == "B":
            self.r = min(self.rows - 1, self.r + n)
        elif final == "C":
            self.c = min(self.cols - 1, self.c + n)
        elif final == "D":
            self.c = max(0, self.c - n)
        elif final == "G":
            self.c = min(self.cols - 1, n - 1)
        elif final in "Hf":
            row = nums[0] if nums and nums[0] else 1
            col = nums[1] if len(nums) > 1 and nums[1] else 1
            self.r, self.c = min(self.rows - 1, row - 1), min(self.cols - 1, col - 1)
        elif final == "J":
            mode = nums[0] if nums else 0
            if mode == 0:
                self.grid[self.r][self.c:] = [" "] * (self.cols - self.c)
                for i in range(self.r + 1, self.rows):
                    self.grid[i] = [" "] * self.cols
            elif mode in (2, 3):
                self.grid = [[" "] * self.cols for _ in range(self.rows)]
        elif final == "K":
            mode = nums[0] if nums else 0
            if mode == 0:
                self.grid[self.r][self.c:] = [" "] * (self.cols - self.c)
            elif mode == 1:
                self.grid[self.r][: self.c + 1] = [" "] * (self.c + 1)
            else:
                self.grid[self.r] = [" "] * self.cols
        elif final == "n" and nums and nums[0] == 6:
            self.reply(f"\x1b[{self.r + 1};{self.c + 1}R")


def run(steps, cwd, wait=0.8, rows=40, cols=120):
    """Runs sparsh, sends each step as keystrokes and returns the emulated
    screen text after startup and after every step."""
    env = dict(os.environ, HOME=tempfile.mkdtemp(), TERM="xterm-256color")
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.execvpe(BIN, ["sparsh"], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    screen = Screen(rows, cols, lambda text: os.write(fd, text.encode()))

    def drain(seconds):
        end = time.time() + seconds
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
            screen.feed(data)
        return screen.text()

    shots = [drain(2.0)]
    for step in steps:
        os.write(fd, step.encode())
        shots.append(drain(wait))
    try:
        os.kill(pid, 9)
    except OSError:
        pass
    return shots


def check(label, steps, cwd, expect, forbid=()):
    screen = run(steps, cwd)[-1]
    ok = all(e in screen for e in expect) and not any(f in screen for f in forbid)
    print(("ok   " if ok else "FAIL ") + label)
    if not ok:
        print(screen)
    return ok


def prompt_line(screen):
    """Text typed after the last prompt marker."""
    lines = [l for l in screen.split("\n") if l.startswith("╰─ ❯ ")]
    return lines[-1][len("╰─ ❯ "):] if lines else ""


def selected_line(screen):
    lines = [l for l in screen.split("\n") if "▶" in l]
    return lines[0] if len(lines) == 1 else None


def check_screen(label, steps, cwd, predicate):
    screen = run(steps, cwd)[-1]
    ok = predicate(screen)
    print(("ok   " if ok else "FAIL ") + label)
    if not ok:
        print(screen)
    return ok


def menu_checks(d):
    left = "\x1b[D"
    tail = '} from "lib.spar";'
    setup = ['struct P { name: str = ""; port: int = 0; };\r', "var p: P = P()\r"]
    down = "\x1b[B"
    imp = ["import { ", tail, left * len(tail), "\t"]

    def boxed(screen):
        return (" ╭──" in screen and "─╮" in screen and " ╰──" in screen and "╯" in screen
                and " ├──" in screen and selected_line(screen) is not None)

    results = [
        check_screen("member menu is a bordered popup with footer and index", setup + ["p.", "\t"], d,
                     lambda s: boxed(s) and "1/4" in s and "name" in s and "port" in s and "method" in s),
        check_screen("down moves the marker", setup + ["p.", "\t", down], d,
                     lambda s: boxed(s) and "port" in (selected_line(s) or "") and "2/4" in s),
        check_screen("enter inserts the selected member", setup + ["p.", "\t", down, "\r"], d,
                     lambda s: prompt_line(s) == "p.port" and selected_line(s) is None and " ╭──" not in s),
        check_screen("shift-tab moves back", setup + ["p.", "\t", down, "\x1b[Z"], d,
                     lambda s: "name" in (selected_line(s) or "") and "1/4" in s),
        check_screen("esc closes the menu and keeps the buffer", setup + ["p.", "\t", down, "\x1b"], d,
                     lambda s: prompt_line(s) == "p." and selected_line(s) is None and " ╭──" not in s),
        check_screen("typing filters the open menu", setup + ["p.", "\t", "p"], d,
                     lambda s: prompt_line(s).startswith("p.p") and "1/1" in s and "port" in s),
        check_screen("import menu is a bordered popup", imp, d,
                     lambda s: boxed(s) and "greet" in s and "port" in s),
        check_screen("import menu enter inserts the selected name", imp + [down, "\r"], d,
                     lambda s: prompt_line(s).startswith("import { ")
                     and any(n in prompt_line(s) for n in ("greet", "port"))
                     and prompt_line(s).endswith(tail) and " ╭──" not in s),
    ]
    return results


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
        check("import names from a package", ['import pkg { ', '} from "std/fs";', left * len('} from "std/fs";'), "\t"], d, ["FileMetadata", "exists", "1/15"]),
        check_screen("long menus scroll to keep the selection visible", ['import pkg { ', '} from "std/fs";', left * len('} from "std/fs";'), "\t"] + ["\x1b[B"] * 12, d,
                     lambda s: "readText" in (selected_line(s) or "") or "13/15" in s and selected_line(s) is not None),
        check("member completion", ['struct P { name: str = ""; port: int = 0; };\r', "var p: P = P()\r", "p.", "\t"], d, ["name", "port"]),
        check("scope completion", ["var count: int = 1\r", "var x = co", "\t"], d, ["count"]),
        check("signature hint", ["fn build(profile: str, release: bool = false) -> str { return profile; };\r", "build(profile: "], d, ["release: bool = false"]),
        check("mixed line runs", ["echo hi; var a: int = 2; a + 1\r"], d, ["hi", "3"]),
    ]
    results += menu_checks(d)
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
