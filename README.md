# Sparsh

Sparsh is an interactive Unix shell built on [Spar](https://github.com/oraclevs/spar) — a real typed language for your rc file, functions, and pipelines instead of Bash string-splicing:

```spar
function build(profile: str) -> shell {
    return shell { cargo build --profile "${profile}"; };
};

build(profile: "release")
```

Bare words still run commands like any shell. The difference shows up the moment you want a function, a typed config, or a structured `ls` you can actually query.

- `spar` is the language, compiler, and runtime.
- `spar-command` is the neutral command-plan representation.
- `spar-process` owns Unix process execution primitives.
- `sparsh` (this repo) is the interactive shell and persistent Spar session.

Spar never depends on Sparsh. Sparsh uses Spar, `spar-command`, and `spar-process` as libraries — native Spar shell syntax is never silently translated to Bash/Zsh/Fish.

## Build

Keep the sibling repositories together because the workspaces use path
dependencies:

```text
project/
  spar/
  spar-command/
  spar-process/
  sparsh/
```

Then:

```sh
cd sparsh
cargo build --release
./target/release/sparsh --version
```

## Command-first interactive model

Bare words are shell commands. Explicit call syntax is Spar:

```text
git status       # external command / builtin / alias resolution
build()          # Spar function call
build            # command named "build", never an implicit function call
```

A direct Spar expression returning `shell` auto-executes. Assignment stores the
shell value without executing it:

```spar
function build(profile: str) -> shell {
    return shell { cargo build --profile "${profile}"; };
};

build(profile: "release")
var plan: shell = build(profile: "release");
```

Spar variables and environment variables remain different namespaces:

```spar
var name: str = "OCC";
echo "${name}";       // Spar expression interpolation
```

```text
export NAME=OCC
echo "$NAME"         # shell environment shorthand
```

## Native Sparsh builtins

The practical stateful/interactive builtin set is owned by Sparsh itself:

```text
cd pwd dirs pushd popd
alias unalias
export unset path hash
type which command builtin
jobs fg bg wait disown kill
history
echo printf read
umask ulimit
source . deactivate reload
exec logout
help repl exit
```

Utilities such as `chmod`, `mkdir`, `touch`, `cp`, `mv`, `rm`, `cat`, `grep`,
`head`, `tail`, `find`, `eza`, `git`, and `cargo` stay external programs and are
resolved through PATH.

Examples:

```text
cd ~/Projects
export EDITOR=nvim
alias gs = git status
printf "one\ntwo\n" | grep two
sleep 30 &
jobs
fg %1
history 20
help source
```

Stateful builtins used as pipeline stages operate on isolated shell-service
state, so `cd /tmp | cat` or `export FOO=x | cat` cannot mutate the parent
interactive session.

## Functions in native pipelines

An explicit Spar function call may be a native pipeline stage when it returns
`shell`:

```spar
function readLog(file: str) -> shell {
    return shell { cat "${file}"; };
};
```

```text
readLog(file: "server.log") | grep error | head -n 10
```

A non-shell Spar value is rejected as a pipeline stage; Sparsh never silently
stringifies arbitrary values into commands.

## Configuration: `~/.sparsh/src/config.spar`

`~/.sparsh` is a Spar package (kind `config`) with one automatically loaded
entry point:

```text
~/.sparsh/
  spar.package.spar        manifest (name, entry, dependencies)
  spar.package.lock.spar   generated lockfile
  src/
    config.spar            loaded automatically
    sparsh-types.spar
    functions.spar
```

`~/.sparsh/src/config.spar` is the only file loaded automatically. The other
files are ordinary Spar modules imported by it. On first launch, Sparsh
migrates an older flat `~/.sparsh` (with `sparsh.spar` at the top) into this
layout: it copies the whole directory to `~/.sparsh.bak` first, moves your
`.spar` files (and any directory containing them) into `src/`, renames
`sparsh.spar` to `config.spar`, and writes the manifest and lockfile. If
`~/.sparsh.bak` already exists, migration is skipped with a notice and the flat
layout keeps working; if a step fails, the original tree is restored.

### Dependencies and `pkg`

Add packages from GitHub or a local path and import them from your config, the
prompt, or scripts:

```text
pkg add myTools github:owner/my-tools@1.0.0
pkg add local path:../my-local-tools
pkg tree
pkg install [--offline]
pkg update [alias]
pkg remove myTools
```

```spar
import pkg { dismantler } from "myTools";
```

Aliases are identifiers (letters, digits, underscores). Requests are
`github:owner/repo@1.4.0`, `github:owner/repo#branch-or-commit`, or
`path:../local-dir`. `pkg` runs against `~/.sparsh` from any directory and
reloads the running session, so a new dependency can be imported immediately.
Startup and imports never use the network: they read the lockfile and the
global package store only. If a locked package is missing, run `pkg install`. Sparsh does not inject a hidden
configuration type package. Public configuration types are normal Spar source;
a complete starter model is in `examples/sparsh-types.spar` and is generated by
`../setup-sparsh-config.sh`.

Canonical Spar syntax uses generic list types such as `List<str>`:

```spar
import type {
    SparshAlias,
    SparshEnvironmentVariable,
    SparshPrompt,
    SparshHistory,
    SparshCompletion,
    SparshConfig
} from "./sparsh-types";

struct Config: SparshConfig {
    aliases = [
        { name: "gs"; command: ["git", "status"]; },
        { name: "ll"; command: ["eza", "-la", "--icons", "--git"]; }
    ];

    environment = [
        { name: "EDITOR"; value: "nvim"; },
        {
            name: "PATH";
            prepend: [
                "/opt/android-sdk/platform-tools",
                "$HOME/DevTools/flutter/bin",
                "$HOME/.cargo/bin",
                "$HOME/.local/bin"
            ];
            append: ["$HOME/.pub-cache/bin"];
        }
    ];

    prompt = {
        showStatus: true;
        showDuration: true;
        path: { enabled: true; };
        git: {
            enabled: true;
            showBranch: true;
            showAheadBehind: true;
            showStaged: true;
            showModified: true;
            showUntracked: true;
            showConflicts: true;
        };
        time: { enabled: true; format: "HH:mm:ss"; };
    };

    history = { maxEntries: 10000; dedupeConsecutive: true; };
    completion = { enabled: true; };
};

function startup() -> shell {
    return shell {
        path append $(npm prefix -g)/bin;
        nitch;
    };
};
```

Spar validates the declared types first. Sparsh then independently validates the
evaluated `Config` section, including rejecting fields that Sparsh does not
implement. Editing `sparsh-types.spar` therefore cannot silently enable a fake
setting.

The rc file is ordinary Spar. Normal functions can live in `functions.spar` and
be selectively imported. `reload` builds a candidate session/configuration and
only swaps it into the live shell when parsing, resolving, type checking,
evaluation, and Sparsh validation all succeed.

Sparsh has one startup mechanism: an optional normal Spar function named
`startup`. It runs once for interactive/login shells after the interactive
editor is initialized and before the first prompt. If it returns `shell`, that
shell plan executes in the same persistent `ShellSession` as commands entered
later. For example:

```spar
function startup() -> shell {
    return shell {
        nitch;
    };
};
```

There is intentionally no `startup.commands` config field. Static environment
configuration belongs in `environment`; imperative or dynamically computed
startup work belongs in `startup()`. The startup hook is not run for `sparsh -c`, remote commands, or piped
noninteractive stdin.

`environment` entries use `value` for fixed variables. `PATH` additionally
supports `prepend` and `append`, which preserve the inherited PATH, expand
`$NAME` references from the current environment, and avoid duplicate path
entries. (`${...}` is Spar's own string-interpolation syntax, so `$HOME` is the
recommended form inside environment strings.) `value` cannot be combined with
`prepend`/`append`. Environment config does not execute command substitution;
dynamic paths such as `$(npm prefix -g)/bin` should be added from `startup()`
with the native `path` builtin.

The default prompt leaves path widths unspecified. It renders the complete cwd
while it fits the actual terminal and only abbreviates components when width is
constrained. Explicit `parentLength`, `maxLastLength`, and `maxWidth` settings
override that default behavior.

A ready-to-copy starting pair lives at `examples/config.spar` and
`examples/sparsh-types.spar`:

```sh
mkdir -p ~/.sparsh/src
cp examples/config.spar ~/.sparsh/src/
cp examples/sparsh-types.spar ~/.sparsh/src/
```

Launch Sparsh and it migrates that into a proper package (manifest and lockfile) on first run.

## Prompt v2

The default two-line prompt is width-aware. It keeps the shape of a long path by
abbreviating parent components rather than replacing the whole prefix with an
opaque ellipsis:

```text
~/Projects/Rust/occ_lang/sparsh/crates/sparsh-core
~/Pr/Ru/oc/sp/cr/sparsh-core
```

As the terminal narrows, parent components shrink further and the final
component receives the largest share of the width budget. The prompt can also
show:

```text
 main +2 ~3 ?1 !1 ↑2 ↓1
```

for branch, staged, modified, untracked, conflicts, commits ahead, and commits
behind. Previous failure status, slow-command duration, and current time are
right-aligned when space allows (customizable, see below). Optional information degrades before the cwd
becomes unreadable.

### Right prompt

The right side of the first line is `status`, then three slots you control.
`status` (`✕ 7` after a failed command, nothing after success) is always first and
is not configurable. Each slot is a template string with `{widget}` placeholders
and an optional color:

```spar
prompt = {
    right: {
        slot1: { text: "{duration}"; color: "yellow"; };
        // Nerd Font glyphs are pasted straight into the text; sparsh does not install fonts.
        slot2: { text: " {cpu}%   {ram}%"; color: "cyan"; style: ["bold"]; };
        slot3: { text: " {date:%a %d %b}   {time:%I:%M %p}"; };
        separator: "  ";
        glyphWidth: 1;
        thresholds: {
            cpu: { warn: 70; critical: 90; };
            battery: { warn: 30; critical: 15; };
        };
    };
};
```

Widgets (`{{` and `}}` write literal braces; `time`/`date` take a `strftime`
format and keep commas, other widgets take comma-separated arguments):

| Widget | Shows | Arguments |
|---|---|---|
| `{time}` | local time, default `%H:%M:%S` | `strftime` format, e.g. `{time:%I:%M %p}` |
| `{date}` | local date, default `%Y-%m-%d` | `strftime` format, e.g. `{date:%a %d %b}` |
| `{duration}` | last command's run time; hidden below `durationThresholdMs` | none |
| `{cpu}` | busy % since the previous prompt | `free` (idle %) |
| `{ram}` | used % | `free`, or a size: `used`, `avail`, `total` |
| `{disk}` | used % of the current directory's filesystem | `free`, a size (`used`, `avail`, `total`), or a path such as `/home` |
| `{battery}` | charge %; hidden with no battery | `icon` (Nerd Font glyph), `state` |
| `{load}` | 1-minute load average | `5` or `15` |
| `{uptime}` | `3d 4h`, `4h 12m` | none |
| `{user}`, `{host}` | login name, short hostname | `host:full` |
| `{jobs}` | background job count; hidden at 0 | none |

Percent widgets print the bare number, so write the `%` yourself. "Remaining
disk percentage" is `{disk:free}`. A widget with nothing to show renders empty,
and a slot whose placeholders are all empty disappears entirely (its text and
separator too), so `"  {jobs}"` leaves no stray glyph at 0 jobs.

Colors are a name (`cyan`, `lightred`, `gray`, ...), `#rrggbb` (downgraded to
256 colors unless `COLORTERM` is `truecolor`/`24bit`), or `"0"`-`"255"`. Styles
are any of `bold`, `dim`, `italic`, `underline`. `cpu`, `ram`, `disk`, `battery`
and `load` turn warning/critical colored automatically; `thresholds` overrides
the defaults (cpu 70/90, ram 80/90, disk 85/95, battery 30/15 where lower is
worse, load = cores and 2x cores). CPU, RAM, disk, battery, load and uptime are
read from `/proc` and `/sys`, so they are Linux-only for now and simply hidden
elsewhere.

When the terminal is narrow, whole slots are dropped from the right (slot3, then
slot2, slot1, and status last) before git and the path shrink. If your terminal
draws Nerd Font glyphs two cells wide, set `glyphWidth: 2`.

Without a `right` section the prompt looks exactly as before (duration and a
clock), and the older `showDuration` and `time` keys keep working; setting both
`right` and those keys makes `right` win.

**A configuration mistake never breaks the shell.** Every problem in the
`prompt` section (including the older keys) is soft: the shell starts, the rest of
your config (aliases, environment, history) still applies, and:

- a slot with a bad template, color or style is drawn as a red `✕ slotN` in its
  place while the other slots keep working;
- any other bad value falls back to its default;
- the problems are printed above the prompt once each time the config loads
  (startup and `reload`), for example:

```text
sparsh: 2 prompt config problems (the shell is unaffected)
  ✕ config.prompt.right.slot2.text: unknown widget 'cpuu'; did you mean 'cpu'?
  ✕ config.prompt.right.glyphWidth: must be between 1 and 2, got 9 (using default)
```

Errors outside the `prompt` section (Spar syntax errors, invalid aliases or
environment entries) still reject the config as before.

`NO_COLOR` disables Sparsh UI colors. External program output is never
recolored or stripped, so `eza --icons`, `bat`, `rg`, `git`, and other ANSI/
Nerd-Font-aware tools pass through unchanged.

## History and completion

Interactive editing uses Reedline with file-backed history, hints, completion,
syntax highlighting, parser-aware multiline validation, and bracketed-paste
review.

### Keybindings

Keybindings are configured in the same `~/.sparsh/src/config.spar` file as the rest of Sparsh. User bindings are layered on top of Reedline/Sparsh defaults, so a matching chord overrides the default while unrelated defaults remain active. Actions are validated names rather than arbitrary closures.

```spar
keybindings: List<SparshKeybinding> = [
    { key: "ctrl+r"; action: "historySearch"; },
    { key: "alt+e"; action: "openEditor"; },
    { key: "ctrl+l"; action: "clearScreen"; }
];
```

Supported modifiers are `ctrl`, `alt`, and `shift`. Supported actions are `completion`, `historyMenu`, `historySearch`, `openEditor`, `clearScreen`, `submit`, `cancel`, `eof`, `previousHistory`, `nextHistory`, `up`, `down`, `left`, `right`, `toStart`, `toEnd`, and `pager`.

### Structured `ls` and the pager

In an interactive session a plain `ls [-a] [-l] [paths]` returns a table (`name`, `type`, `size`, `modified`; `-l` adds `mode`, `user`, `group`, `target`). Hidden files are always listed, directories come first, and `type` is one of `dir`, `file`, `exe`, `symlink`, `fifo`, `socket`, `block`, `char`. Anything more complex (pipes, globs, other flags) still runs the external `ls`, and `command ls` bypasses the table. The listing is an ordinary value, so `ls |> take(5)`, `ls |> to yaml` and `_ |> to json` work.

Tables show 50 rows (`SPARSH_MAX_ROWS`) and encoded output 80 lines (`SPARSH_MAX_LINES`). The footer then says `type `view` to page through all of them`. `view`, or `alt+v` at the prompt, opens a full-screen pager over that result with no row, line or width limit. Table headers stay pinned and wide tables scroll sideways.

Default pager keys: `j`/`down`/`enter` line down, `k`/`up` line up, `space`/`pagedown`/`ctrl+f` page down, `b`/`pageup`/`ctrl+b` page up, `d`/`ctrl+d` and `u`/`ctrl+u` half pages, `g`/`home` top, `G`/`end` bottom, `h`/`left` and `l`/`right` sideways, `/` search, `n`/`N` next/previous match, `q`/`esc`/`ctrl+c` quit. Add or replace keys with `pagerKeybindings`; a binding on an existing chord replaces the default:

```spar
pagerKeybindings: List<SparshPagerKeybinding> = [
    { key: "ctrl+n"; action: "lineDown"; },
    { key: "ctrl+p"; action: "lineUp"; },
    { key: "space"; action: "halfPageDown"; }
];
```

Pager actions: `lineDown`, `lineUp`, `pageDown`, `pageUp`, `halfPageDown`, `halfPageUp`, `top`, `bottom`, `left`, `right`, `search`, `searchNext`, `searchPrevious`, `quit`.


Default history location:

```text
$XDG_STATE_HOME/sparsh/history
```

or:

```text
$HOME/.local/state/sparsh/history
```

Completion combines builtins, aliases, cached PATH executables, filesystem
paths, and identifiers from the persistent Spar session. Filesystem completion
works in ordinary command arguments (`ls Pro<Tab>`), `cd`, after pipelines, and
for `~/...` paths. Quoted/spaced directory completion remains open so deeper
components can continue completing.

Use `hash -r` after installing a new executable into an already scanned PATH
directory.

## `source` and `.`

For Spar files:

```text
source ~/.sparsh/src/functions.spar
. ~/.sparsh/src/functions.spar
```

The file is evaluated into the current persistent Spar session. Variables and
functions remain available afterwards, and relative imports resolve from the
sourced file's directory.

For a foreign shell activation script:

```text
source .venv/bin/activate
source --shell bash ./script
source --shell zsh ./script
```

Sparsh runs the foreign source operation in a child shell and atomically imports
only the resulting environment and working directory. This supports Python
virtual-environment activation without pretending Bash aliases/functions/traps
are Spar constructs.

When `source` activates a Python environment by setting `VIRTUAL_ENV`, Sparsh
records the environment changes made by that activation. Use the native builtin:

```text
deactivate
```

to restore those activation-owned environment entries (including `PATH`) while
preserving unrelated environment changes made later in the session. The
standard spelling is `deactivate`; a mistyped `deactive` is offered
`deactivate` by command suggestions.

## Executable `.spar` programs

Inside Sparsh, an executable `.spar` file without a foreign shebang is routed to
the Spar runtime rather than `/bin/sh`:

```sh
chmod +x main.spar
./main.spar
```

A Spar shebang is also supported:

```spar
#!/usr/bin/env spar
function main() -> int { return 0; };
```

An explicit foreign shebang such as `#!/bin/sh` is respected. The executable
bit is still required for `./file.spar`. Executable Spar programs may
participate in redirects and pipelines.

## Job control

On Unix, foreground/background external commands and pipelines use process
groups. Interactive Sparsh transfers the controlling terminal to the foreground
process group and restores it afterwards.

```text
sleep 30 &
jobs
fg %1
bg %1
wait %1
kill -TERM %1
disown %1
```

The implementation is Linux-first. Use the PTY/manual checks in `VERIFY.md`
before selecting Sparsh as a login shell.

## Async functions and HTTP at the prompt

`await` is a shell keyword. Async functions return a promise; put `await` in front of the call to wait for it:

```
import pkg { get } from "std/http";
await get(url: "https://pokeapi.co/api/v2/pokemon/ditto")
_.json()
```

The response is shown as a status line and the body: JSON becomes a table or tree, HTML/XML/text is shown as text. `_` holds the response, so `_.status`, `_.body`, `_.contentType` and `_.json()` work. `await` cannot be used inside a declaration; see `spar/docs/async-await.md`.

The `std/data` functions (`where`, `map`, `take`, ...) are available at the interactive prompt without an import. Scripts still import them.

## Multiline Spar

Normal command mode submits complete input immediately and keeps structurally
incomplete Spar open for more lines. `repl` enters an explicit multiline Spar
editor; Ctrl-D returns to command mode. Bracketed multiline paste gets an
execute/edit/cancel review step. In normal command mode, executing a reviewed
paste runs complete command lines sequentially in the same persistent session,
while structurally incomplete Spar constructs stay grouped until complete.

## Startup modes

```sh
sparsh
sparsh -c 'git status'
printf 'pwd\n' | sparsh
sparsh --login
sparsh --remote-command 'pwd'
```

`--help` and `--version` do not initialize a shell session.

## Verification status

The current assembly environment cannot run Rust. Before treating a generated
patch as verified, run the exact compiler/test/build/PTy checklist in the
project-root `VERIFY.md` on a machine with the pinned Rust toolchain.
