# Sparsh

Sparsh is the standalone Unix shell for the Spar ecosystem. The products stay
separate:

- `spar` is the Spar compiler, runtime, task runner, and package manager.
- `sparsh` is the interactive shell and persistent Spar environment.

This repository currently contains the first core milestone. It is useful for
testing command execution and persistent Spar state, but it is not yet a
daily-driver shell.

## Install

The sibling `spar`, `spar-command`, and `spar-process` repositories must be
present beside this repository while path dependencies are in use.

```text
cargo install --path .
sparsh --version
```

## Commands

Sparsh parses commands with Spar's native structural command grammar and runs
them through the shared `spar-process` runtime. It does not invoke `/bin/sh`
or Bash.

```text
sparsh -c 'pwd'
sparsh -c 'printf hello | grep hello'
sparsh -c 'cargo test > test.log'
```

The current grammar supports ordinary external commands, quoted arguments,
sequences, pipelines, stdout truncation and append redirection, and stderr
redirection. Child stdout and stderr are inherited unchanged.

The initial builtin registry contains:

- `cd [directory]`
- `pwd`
- `exit [status]`

Standalone builtins run in the real shell session, so `cd` affects later
commands. A builtin name inside a pipeline is treated as an external command
in this milestone.

## Persistent Spar state

Start `sparsh` and enter one-line Spar declarations or mutations:

```text
❯ var project: str = "spar";
❯ project
"spar"
```

Dispatch is deterministic:

- A bare unknown word such as `build` uses Unix command resolution.
- A single bare identifier renders a Spar value only when that variable
  already exists in the current session.
- Explicit call syntax such as `build()` is Spar.
- Assignments are Spar when their left-hand variable already exists.

Strings and stored `shell` plans are data. Rendering either one never executes
its contents.

## Current milestone limitations

The following required Sparsh systems are not implemented yet:

- Unix job control, process groups, terminal handoff, `&`, Ctrl+C/Ctrl+Z job
  routing, `jobs`, `fg`, and `bg`;
- aliases, environment and structured PATH services, executable caching, and
  the directory stack;
- transactional Spar configuration, safe mode, diagnostics, and rollback;
- persistent history, completion, autosuggestions, and semantic highlighting;
- `NormalEditor`, `ReplEditor`, `PasteReview`, bracketed paste, and the rich
  prompt;
- automatic execution of a `shell` value returned directly by an interactive
  Spar function call;
- typed command interpolation, explicit list expansion, input redirection,
  logical `&&`/`||`, and background `&` syntax.

Until job control and PTY tests are complete, do not configure Sparsh as a
login shell or expect full-screen interactive programs to behave like they do
under Bash or Zsh.
