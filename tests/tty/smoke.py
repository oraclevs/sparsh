#!/usr/bin/env python3
"""Drives the real sparsh binary through a pseudo-terminal and checks the
completion features that unit tests cannot see (the editor decides what text
the completer receives). Usage: SPARSH_BIN=target/release/sparsh python3 tests/tty/smoke.py"""
import fcntl, os, pty, re, select, struct, sys, tempfile, termios, time

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


def run(steps, cwd, wait=0.8, rows=40, cols=120, home=None, env_overrides=None, include_raw=False):
    """Runs sparsh, sends each step as keystrokes and returns the emulated
    screen text after startup and after every step."""
    env = dict(os.environ, HOME=str(home or tempfile.mkdtemp()), TERM="xterm-256color")
    env.pop("NO_COLOR", None)
    if env_overrides:
        env.update(env_overrides)
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.execvpe(BIN, ["sparsh"], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    screen = Screen(rows, cols, lambda text: os.write(fd, text.encode()))

    def drain(seconds):
        raw = bytearray()
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
            raw.extend(data)
            screen.feed(data)
        return screen.text(), bytes(raw)

    first, first_raw = drain(2.0)
    shots, captures = [first], [first_raw]
    for step in steps:
        if callable(step):
            step(env["HOME"])
        else:
            os.write(fd, step.encode())
        shot, raw = drain(wait)
        shots.append(shot)
        captures.append(raw)
    try:
        os.kill(pid, 9)
    except OSError:
        pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass
    os.close(fd)
    return (shots, captures) if include_raw else shots


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


def theme_source(base):
    return f'''var themeState: Record = {{
    source: {{ kind: "external"; origin: "smoke"; }};
    theme: {{
        prompt: {{ cwd: {{ foreground: "{base + 1}"; }}; }};
        syntax: {{ functionName: {{ foreground: "{base + 2}"; }}; }};
        completion: {{
            menuSelected: {{ background: "{base + 3}"; }};
            hintSignature: {{ foreground: "{base + 4}"; }};
        }};
        data: {{ tableHeader: {{ foreground: "{base + 5}"; }}; }};
    }};
}};
'''


def theme_checks(cwd):
    with tempfile.TemporaryDirectory() as home:
        src = os.path.join(home, ".sparsh", "src")
        os.makedirs(src)
        active = os.path.join(src, "theme.generated.spar")
        with open(active, "w") as target:
            target.write(theme_source(200))

        def rewrite(base):
            def apply(_):
                with open(active, "w") as target:
                    target.write(theme_source(base))
            return apply

        def partial(_):
            with open(active, "w") as target:
                target.write("var themeState: Record = {")

        steps = [
            'fn build(profile: str) -> str { return profile; };\r',
            'build(profile: ', '\x03', 'b\t', '\x03', 'dirs\r',
            rewrite(210), '\r', 'build(profile: ', '\x03', 'b\t', '\x03', 'dirs\r',
            partial, '\r', rewrite(220), '\r',
        ]
        _, raw = run(steps, cwd, wait=1.0, home=home,
                     env_overrides={"COLORTERM": ""}, include_raw=True)
        cases = [
            ("theme prompt starts with file role", b"38;5;201", raw[0]),
            ("theme syntax reaches editor", b"38;5;202", raw[1] + raw[2]),
            ("theme signature hint uses role", b"38;5;204", raw[2]),
            ("theme menu background uses role", b"48;5;203", raw[4]),
            ("theme table header uses role", b"38;5;205", raw[6]),
            ("theme prompt reloads after rewrite", b"38;5;211", raw[8]),
            ("theme syntax reloads", b"38;5;212", raw[9]),
            ("theme hint reloads", b"38;5;214", raw[9]),
            ("theme menu reloads", b"48;5;213", raw[11]),
            ("theme table reloads", b"38;5;215", raw[13]),
            ("bad rewrite keeps last valid theme", b"38;5;211", raw[15]),
            ("valid rewrite recovers", b"38;5;221", raw[17]),
        ]
        results = []
        for label, token, output in cases:
            ok = token in output
            print(("ok   " if ok else "FAIL ") + label)
            if not ok:
                print(output[-1000:].decode("utf8", "replace"))
            results.append(ok)
        _, plain_raw = run(['dirs\r'], cwd, wait=0.6, home=home,
                           env_overrides={"NO_COLOR": "1", "COLORTERM": ""}, include_raw=True)
        no_color = not any(re.search(rb"\x1b\[(?!0(?:;0)*m)[0-9][0-9;]*m", output) for output in plain_raw)
        print(("ok   " if no_color else "FAIL ") + "NO_COLOR suppresses theme styles")
        results.append(no_color)
        return results


def theme_menu_check(cwd):
    """`theme set <Tab>` offers the themes registered in configuration.themes."""
    with tempfile.TemporaryDirectory() as home:
        src = os.path.join(home, ".sparsh", "src")
        os.makedirs(src)
        schema = os.path.join(os.path.dirname(__file__), "..", "..", "examples", "sparsh-types.spar")
        with open(schema) as source, open(os.path.join(src, "sparsh-types.spar"), "w") as target:
            target.write(source.read())
        with open(os.path.join(src, "config.spar"), "w") as target:
            target.write('''import { SparshConfig, SparshThemeKind, SparshTheme } from "./sparsh-types.spar";
var configuration: SparshConfig = SparshConfig(
    themes: some(value: [
        SparshThemeKind(name: "gruvbox", theme: SparshTheme()),
        SparshThemeKind(name: "royal", theme: SparshTheme()),
    ]),
);
''')
        screen = run(["theme set ", "\t"], cwd, wait=1.0, home=home)[-1]
        ok = all(name in screen for name in ["gruvbox", "royal", "default", "--accent"])
        print(("ok   " if ok else "FAIL ") + "theme set offers registered theme names")
        if not ok:
            print(screen)
        table = run(["theme list\r"], cwd, wait=1.0, home=home)[-1]
        shown = ("name" in table and "description" in table and "active" in table
                 and "gruvbox" in table and "royal" in table and "│" in table)
        print(("ok   " if shown else "FAIL ") + "theme list is a table")
        if not shown:
            print(table)
        return [ok, shown]


def startup_builtin_check(cwd):
    """A startup() that calls state-changing builtins sees them take effect."""
    with tempfile.TemporaryDirectory() as home:
        src = os.path.join(home, ".sparsh", "src")
        os.makedirs(src)
        with open(os.path.join(src, "config.spar"), "w") as target:
            target.write('''fn startup() -> ShellResult<int, int> {
    path append "/tmp/sparsh-smoke-startup-dir";
    export SPARSH_SMOKE_STARTUP=from_startup;
    return ok(value: 0, quiet: true);
};
''')
        screen = run(["path\r", "echo $SPARSH_SMOKE_STARTUP\r"], cwd, wait=1.2, home=home)[-1]
        ok = ("/tmp/sparsh-smoke-startup-dir" in screen and "from_startup" in screen
              and "could not run" not in screen)
        print(("ok   " if ok else "FAIL ") + "startup can call path and export")
        if not ok:
            print(screen)
        return [ok]


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
    results += theme_checks(d)
    results += theme_menu_check(d)
    results += startup_builtin_check(d)
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
