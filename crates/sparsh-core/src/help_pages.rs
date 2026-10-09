//! Long-form help for builtins: what it does, every form, and examples that
//! can be typed as shown. The terminal draws a `HelpPage` with colors; plain
//! sessions get `render_text`.

use crate::builtin::BuiltinRegistry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpPage {
    pub name: String,
    pub summary: String,
    pub details: String,
    pub usage: Vec<String>,
    /// (flag or form, what it does)
    pub options: Vec<(String, String)>,
    /// (command to type, what happens)
    pub examples: Vec<(String, String)>,
    pub see_also: Vec<String>,
}

type Pair = (&'static str, &'static str);

fn page(
    name: &str,
    summary: &str,
    details: &str,
    usage: &[&str],
    options: &[Pair],
    examples: &[Pair],
    see_also: &[&str],
) -> HelpPage {
    let owned = |pairs: &[Pair]| pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    HelpPage {
        name: name.into(),
        summary: summary.into(),
        details: details.into(),
        usage: usage.iter().map(|line| line.to_string()).collect(),
        options: owned(options),
        examples: owned(examples),
        see_also: see_also.iter().map(|name| name.to_string()).collect(),
    }
}

/// The page for `name`, written by hand when one exists and otherwise built
/// from the builtin's registered description and usage.
pub fn help_page(name: &str, registry: &BuiltinRegistry) -> Option<HelpPage> {
    let entry = registry.find(name)?;
    Some(written(entry.name).unwrap_or_else(|| {
        page(entry.name, entry.description, "", &[entry.usage], &[], &[], &[])
    }))
}

fn written(name: &str) -> Option<HelpPage> {
    Some(match name {
        "cd" => page(
            "cd",
            "Change the current directory",
            "With no argument it goes to your home directory. `cd -` returns to the previous directory. Relative paths are resolved from where you are now.",
            &["cd [directory]"],
            &[("cd", "go to your home directory"), ("cd -", "go back to the previous directory"), ("cd ~/Projects", "`~` stands for your home directory")],
            &[("cd /etc", "go to an absolute path"), ("cd ..", "go up one level"), ("cd -", "jump back to wherever you just were")],
            &["z", "pushd", "pwd"],
        ),
        "z" => page(
            "z",
            "Jump to a directory you have visited before",
            "Sparsh remembers every directory you enter. Type part of a name and `z` takes you to the best match, ranked by how often and how recently you were there. It never guesses: only directories you have actually entered are candidates. Press Tab after `z` to see them.",
            &["z [words...]", "z --set NAME [DIR]", "z --unset NAME", "z --list"],
            &[
                ("z foo bar", "words must appear in that order in the path; the last one must be in the final folder name"),
                ("z --set NAME [DIR]", "give DIR (default: the current directory) a short name"),
                ("z --unset NAME", "remove a short name"),
                ("z --list", "show every remembered directory with visits and last visit"),
                ("z /path or z ~/x or z -", "behaves like cd"),
            ],
            &[
                ("z occ", "jump to the best directory whose name matches `occ`"),
                ("z proj spar", "a directory called spar somewhere under something matching proj"),
                ("z --set api ~/Projects/Rust/kudicall-backend", "from now on `z api` goes there, and Tab shows `api`"),
                ("z --list", "see everything z knows, best first"),
            ],
            &["cd", "dirs", "history"],
        ),
        "pwd" => page("pwd", "Print the current directory", "Prints the full path of the directory you are in.", &["pwd"], &[], &[("pwd", "prints for example /home/occ/Projects")], &["cd", "dirs"]),
        "pushd" => page(
            "pushd",
            "Go to a directory and remember where you were",
            "Changes directory like `cd`, and pushes the old directory on a stack you can return to with `popd`. See the stack with `dirs`.",
            &["pushd [directory]"],
            &[],
            &[("pushd /var/log", "go to /var/log, remembering the current directory"), ("popd", "return to the remembered directory")],
            &["popd", "dirs", "cd"],
        ),
        "popd" => page("popd", "Return to the directory saved by pushd", "Removes the top directory from the stack and changes to it.", &["popd"], &[], &[("pushd /tmp", "go to /tmp and save where you were"), ("popd", "come back")], &["pushd", "dirs"]),
        "dirs" => page("dirs", "Show the directory stack", "Lists the current directory followed by the directories saved by `pushd`. At a terminal it is shown as a table.", &["dirs"], &[], &[("dirs", "see where popd would take you")], &["pushd", "popd"]),
        "alias" => page(
            "alias",
            "Define or list command shortcuts",
            "An alias replaces the first word of a command with a longer command. With no arguments, lists every alias (as a table at a terminal). Put aliases you want every session in your config file.",
            &["alias", "alias name = command [arguments...]"],
            &[("alias", "list all aliases"), ("alias name = command ...", "define or replace an alias")],
            &[("alias gs = git status", "`gs` now runs `git status`"), ("alias ll = ls -l", "a shortcut for a long listing"), ("alias", "show what is defined")],
            &["unalias", "type"],
        ),
        "unalias" => page("unalias", "Remove aliases", "Deletes one or more aliases from this session.", &["unalias name [...]"], &[], &[("unalias gs", "remove the `gs` alias"), ("unalias gs ll", "remove several at once")], &["alias"]),
        "export" => page(
            "export",
            "Set or list environment variables",
            "Variables set with `export` are passed to every program you start. With no arguments it lists the whole environment (as a table at a terminal).",
            &["export", "export NAME=value [NAME2=value ...]", "export NAME"],
            &[("NAME=value", "set the variable"), ("NAME", "make sure NAME exists in the environment")],
            &[("export EDITOR=nvim", "programs now see EDITOR=nvim"), ("export", "list all variables")],
            &["unset", "path"],
        ),
        "unset" => page("unset", "Remove environment variables", "Removes variables from the environment of this session.", &["unset NAME [...]"], &[], &[("unset HTTP_PROXY", "stop passing HTTP_PROXY to programs")], &["export"]),
        "path" => page(
            "path",
            "Look at or change PATH",
            "PATH is the list of directories searched for programs. With no argument it lists them and whether each exists. Changes last for this session; put them in your config to keep them.",
            &["path", "path prepend DIR", "path append DIR", "path remove DIR"],
            &[("prepend DIR", "search DIR first"), ("append DIR", "search DIR last"), ("remove DIR", "stop searching DIR")],
            &[("path prepend ~/.cargo/bin", "programs installed by cargo win over system ones"), ("path", "list the search directories")],
            &["hash", "which", "export"],
        ),
        "hash" => page(
            "hash",
            "Show or clear the remembered program locations",
            "Sparsh remembers where it found each program. `hash` lists them; `hash -r` forgets them, which helps after installing something new.",
            &["hash", "hash -r"],
            &[("-r", "forget all remembered locations")],
            &[("hash", "list known program locations"), ("hash -r", "re-scan PATH on next use")],
            &["path", "which"],
        ),
        "type" => page(
            "type",
            "Say what a name means",
            "Tells you whether a name is an alias, a Sparsh builtin or an external program (and where it lives).",
            &["type name [...]"],
            &[],
            &[("type ls", "see whether ls is an alias, a builtin or a program"), ("type cd z", "describe several names")],
            &["which", "alias"],
        ),
        "which" => page("which", "Find where a program lives", "Prints the path of an external program, searching PATH.", &["which name [...]"], &[], &[("which cargo", "prints the full path of cargo")], &["type", "path"]),
        "command" => page("command", "Run a command ignoring aliases", "Runs `name` as if no alias with that name existed.", &["command name [argument ...]"], &[], &[("command ls -la", "run the real ls even if `ls` is aliased")], &["builtin", "alias"]),
        "builtin" => page("builtin", "Run only a Sparsh builtin", "Runs `name` as a builtin, skipping aliases and programs of the same name.", &["builtin name [argument ...]"], &[], &[("builtin cd /tmp", "use the builtin cd for sure")], &["command"]),
        "jobs" => page("jobs", "List background and stopped jobs", "Shows the jobs this shell started (running, stopped or done). At a terminal it is shown as a table.", &["jobs"], &[], &[("sleep 60 &", "start a background job"), ("jobs", "see it listed")], &["fg", "bg", "kill", "wait"]),
        "fg" => page("fg", "Bring a job to the foreground", "Resumes a job and waits for it. With no argument it uses the most recent job.", &["fg [%job]"], &[("%1", "the job number shown by `jobs`")], &[("fg", "foreground the latest job"), ("fg %2", "foreground job 2")], &["bg", "jobs"]),
        "bg" => page("bg", "Continue a stopped job in the background", "Resumes a stopped job without waiting for it.", &["bg [%job]"], &[("%1", "the job number shown by `jobs`")], &[("bg %1", "let job 1 keep running behind the prompt")], &["fg", "jobs"]),
        "wait" => page("wait", "Wait for a background job to finish", "Blocks until the job exits.", &["wait [%job]"], &[], &[("wait %1", "pause here until job 1 is done")], &["jobs", "fg"]),
        "disown" => page("disown", "Stop managing a job", "The job keeps running but the shell no longer tracks it or waits for it.", &["disown [%job]"], &[], &[("disown %1", "forget job 1 without stopping it")], &["jobs"]),
        "kill" => page(
            "kill",
            "Send a signal to a job or process",
            "Sends a signal (default TERM) to a job (`%1`) or a process id.",
            &["kill [-SIGNAL] target [...]"],
            &[("-SIGNAL", "for example -9 or -KILL"), ("%N", "a job number"), ("PID", "a process id")],
            &[("kill %1", "stop job 1"), ("kill -9 4242", "force-stop process 4242")],
            &["jobs"],
        ),
        "history" => page(
            "history",
            "List, search or remove command history",
            "History is saved as structured records: when you ran each command, where, and what kind it was. At a terminal the listing is a table. Commands run in stealth mode are never saved.",
            &["history [N]", "history --search TEXT", "history --delete LINE", "history --delete-matching TEXT", "history --delete-exact COMMAND", "history --clear"],
            &[
                ("N", "only the last N entries"),
                ("--search TEXT", "entries containing TEXT"),
                ("--delete LINE", "remove one entry by its line number"),
                ("--delete-matching TEXT", "remove every entry containing TEXT"),
                ("--delete-exact COMMAND", "remove entries equal to COMMAND"),
                ("--clear", "forget all commands and visited directories"),
            ],
            &[("history 20", "the last twenty commands"), ("history --search docker", "everything you ran with docker"), ("history --delete-matching password", "scrub secrets typed by mistake")],
            &["stealth", "z"],
        ),
        "stealth" => page("stealth", "Turn private mode on or off", "While stealth is on, nothing you type is saved to history and no directories are remembered.", &["stealth [on|off|status]"], &[("on", "stop recording"), ("off", "record again"), ("status", "show the current mode")], &[("stealth on", "stop saving history"), ("stealth off", "resume")], &["history"]),
        "echo" => page("echo", "Print arguments", "Writes its arguments separated by spaces.", &["echo [-n] [argument ...]"], &[("-n", "do not print the final newline")], &[("echo hello world", "prints hello world"), ("echo -n hi", "prints hi with no newline")], &["printf"]),
        "ls" => page(
            "ls",
            "List directory contents",
            "At a terminal the listing is a table you can filter with `_`. Inside pipes, chains and scripts you get plain text. Dates are shown in local time.",
            &["ls [-a] [-l] [--sizes] [path ...]"],
            &[("-a", "include hidden files"), ("-l", "long format: mode, owner, size"), ("--sizes", "total the size of each directory (slow)"), ("path", "one or more paths to list")],
            &[("ls", "files in the current directory"), ("ls -la ~/Projects", "everything in Projects with details"), ("ls --sizes", "how big each folder is")],
            &["ll", "cd"],
        ),
        "ll" => page("ll", "Long directory listing", "Same as `ls -l`.", &["ll [-a] [--sizes] [path ...]"], &[("-a", "include hidden files"), ("--sizes", "total directory sizes (slow)")], &[("ll", "details for the current directory")], &["ls"]),
        "printf" => page("printf", "Print formatted text", "Formats arguments like C printf: %s for text, %d for integers, \\n for a newline.", &["printf format [argument ...]"], &[], &[("printf '%s is %d\\n' age 33", "prints: age is 33")], &["echo"]),
        "read" => page("read", "Read a line into a variable", "Waits for one line of input and stores it in an environment variable.", &["read [-r] NAME"], &[("-r", "keep backslashes as typed")], &[("read NAME", "type a line; $NAME now holds it")], &["export"]),
        "umask" => page("umask", "Show or set the file-creation mask", "The mask removes permissions from newly created files.", &["umask [0000-0777]"], &[], &[("umask", "show the current mask"), ("umask 077", "new files are private to you")], &[]),
        "ulimit" => page("ulimit", "Show or set resource limits", "Limits apply to programs started from this shell.", &["ulimit -a", "ulimit -n|-c|-s|-u [value|unlimited]"], &[("-a", "show all"), ("-n", "open files"), ("-c", "core file size"), ("-s", "stack size"), ("-u", "processes")], &[("ulimit -n", "show the open-file limit"), ("ulimit -n 4096", "raise it")], &[]),
        "help" => page("help", "Explain a builtin", "With no argument it lists every builtin as a table. With a name it shows what the command does, its forms, and examples you can copy.", &["help", "help builtin"], &[], &[("help", "list all builtins"), ("help z", "learn how z works"), ("help history", "see every history option")], &["type"]),
        "source" => page("source", "Run a file in this session", "Runs a `.spar` file (or a shell script) so its functions, variables and environment changes stay in the current session.", &["source file"], &[], &[("source ./env.spar", "load the functions and variables from that file")], &["reload"]),
        "exec" => page("exec", "Replace the shell with a program", "Sparsh is replaced by the program; when it exits you are back in your terminal.", &["exec command [argument ...]"], &[], &[("exec zsh", "switch this terminal to zsh")], &["exit"]),
        "exit" => page("exit", "Leave the shell", "Optionally with an exit status; without one it uses the status of the last command.", &["exit [status]"], &[], &[("exit", "quit"), ("exit 3", "quit with status 3")], &["logout"]),
        "logout" => page("logout", "Leave a login shell", "Like `exit`, for login shells.", &["logout"], &[], &[("logout", "end the login session")], &["exit"]),
        "deactivate" => page("deactivate", "Leave the active Python virtual environment", "Undoes the changes a virtual environment made to PATH and the environment.", &["deactivate"], &[], &[("deactivate", "back to the system Python")], &[]),
        "repl" => page(
            "repl",
            "Edit everything you have declared in this session",
            "Opens the imports, variables, structs and functions you typed or sourced as one Spar file in Sparsh's editor, with highlighting, indentation, Tab completion and live error checking. Press Ctrl-S to save: the whole file is compiled together with your config, and only if it compiles does it replace the live session. Otherwise nothing changes, the errors are shown, and you can fix them and try again. Nothing is written to disk.",
            &["repl", "repl --editor"],
            &[("Ctrl-S", "validate and apply"), ("Tab", "indent at the start of a line, otherwise complete"), ("Shift+arrows", "select; Ctrl-C copy, Ctrl-X cut, Ctrl-V paste, Alt-A select all"), ("--editor", "edit in the editor named by EDITOR (or VISUAL) instead; same validation on save"), ("Esc", "close without changes")],
            &[
                ("repl", "open the session as a file"),
                ("repl --editor", "open it in nvim, code --wait, or whatever EDITOR is"),
                ("delete an `import { x } from \"lib.spar\";` line, save", "drop an import whose function no longer exists"),
                ("change `var port: int = 80;` to 8080, save", "re-declare a variable with a new value"),
            ],
            &["srepl", "source", "reload"],
        ),
        "srepl" => page(
            "srepl",
            "A private multiline Spar REPL",
            "Type Spar over several lines; an empty line runs the block. While it is open, stealth is on: nothing you type is saved to history and no directories are remembered. When you leave with Ctrl-D, everything you declared inside is discarded and stealth returns to what it was, so the session is exactly as before.",
            &["srepl"],
            &[("empty line", "run the block you typed"), ("Ctrl-D", "leave and discard everything declared here")],
            &[("srepl", "start a private scratch session"), ("var token: str = \"...\";", "declared only inside the private REPL, gone after Ctrl-D")],
            &["repl", "stealth", "history"],
        ),
        "reload" => page("reload", "Reload your config", "Re-reads ~/.sparsh/src/config.spar. If it has an error nothing changes and you keep the working config.", &["reload"], &[], &[("reload", "apply edits to config.spar without restarting")], &["source", "pkg"]),
        "pkg" => page(
            "pkg",
            "Manage the packages your config uses",
            "Your config at ~/.sparsh is a Spar package; `pkg` edits its dependencies.",
            &["pkg add <alias> <request>", "pkg remove <alias>", "pkg install [--offline]", "pkg update [alias]", "pkg tree"],
            &[("add", "add a dependency under an alias"), ("remove", "drop a dependency"), ("install", "fetch what the lock file needs (--offline uses the cache)"), ("update", "update one or all dependencies"), ("tree", "show the dependency tree")],
            &[("pkg tree", "see what your config depends on"), ("pkg install --offline", "install from cache only")],
            &["reload"],
        ),
        _ => return None,
    })
}

/// Plain-text rendering for scripts, pipes and `sparsh -c`.
pub fn render_text(page: &HelpPage) -> String {
    let mut out = format!("{} — {}\n", page.name, page.summary);
    if !page.details.is_empty() {
        out.push_str(&format!("\n{}\n", page.details));
    }
    out.push('\n');
    out.push_str(if page.usage.len() == 1 { "usage: " } else { "usage:\n  " });
    out.push_str(&page.usage.join("\n  "));
    out.push('\n');
    if !page.options.is_empty() {
        out.push_str("\noptions:\n");
        for (flag, meaning) in &page.options {
            out.push_str(&format!("  {flag:<28} {meaning}\n"));
        }
    }
    if !page.examples.is_empty() {
        out.push_str("\nexamples:\n");
        for (example, meaning) in &page.examples {
            out.push_str(&format!("  $ {example}\n      {meaning}\n"));
        }
    }
    if !page.see_also.is_empty() {
        out.push_str(&format!("\nsee also: {}\n", page.see_also.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_has_a_page_and_hand_written_ones_are_complete() {
        let registry = BuiltinRegistry::new();
        for metadata in registry.metadata() {
            let page = help_page(metadata.name, &registry).expect(metadata.name);
            assert!(!page.usage.is_empty(), "{}", metadata.name);
            let text = render_text(&page);
            assert!(text.contains("usage"), "{}", metadata.name);
        }
        for name in ["cd", "z", "history", "alias", "ls", "jobs", "path"] {
            let page = written(name).expect(name);
            assert!(!page.details.is_empty() && !page.examples.is_empty(), "{name}");
        }
    }

    #[test]
    fn unknown_names_have_no_page() {
        assert!(help_page("nope", &BuiltinRegistry::new()).is_none());
    }
}
