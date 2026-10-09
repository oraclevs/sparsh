//! What a command's arguments are: the table behind context-aware path completion.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArgumentKind {
    Files,
    Directories,
    /// `z`: remembered directories, not the file system.
    Frecent,
    FilesAndDirectories,
    Nothing,
}

/// True when the command has an explicit entry in the table.
pub(crate) fn is_known_command(command: &str) -> bool {
    explicit_kind(command).is_some()
}

fn explicit_kind(command: &str) -> Option<ArgumentKind> {
    Some(match command {
        "cat" | "less" | "more" | "head" | "tail" | "wc" | "bat" | "source" | "." | "diff"
        | "sha256sum" | "md5sum" | "sha1sum" | "sha512sum" | "b3sum" | "nl" | "tac" | "strings"
        | "file" | "xxd" | "od" => ArgumentKind::Files,
        "cd" | "pushd" | "rmdir" => ArgumentKind::Directories,
        "z" => ArgumentKind::Frecent,
        "echo" | "printf" | "kill" | "which" | "type" | "man" | "alias" | "export" | "unset"
        | "history" => ArgumentKind::Nothing,
        "cp" | "mv" | "rm" | "ls" | "chmod" | "stat" | "du" | "mkdir" => {
            ArgumentKind::FilesAndDirectories
        }
        _ => return None,
    })
}

pub(crate) fn argument_kind(command: &str, _argument_index: usize) -> ArgumentKind {
    explicit_kind(command).unwrap_or(ArgumentKind::FilesAndDirectories)
}

/// Words after the command with leading option words (`-x`, `--long`, `--k=v`) removed
/// from the count; returns how many positional words precede the cursor word.
pub(crate) fn positional_index(words_after_command: &[&str]) -> usize {
    let mut count = 0;
    let mut after_double_dash = false;
    for word in words_after_command {
        if after_double_dash {
            count += 1;
        } else if *word == "--" {
            after_double_dash = true;
        } else if word.len() > 1 && word.starts_with('-') {
            // option word
        } else {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_commands_have_expected_kinds() {
        for c in [
            "cat", "less", "more", "head", "tail", "wc", "bat", "source", "diff",
        ] {
            assert_eq!(argument_kind(c, 0), ArgumentKind::Files, "{c}");
        }
        for c in ["cd", "pushd", "rmdir"] {
            assert_eq!(argument_kind(c, 0), ArgumentKind::Directories, "{c}");
        }
        for c in ["cp", "mv", "rm", "ls", "chmod", "stat", "du", "mkdir"] {
            assert_eq!(
                argument_kind(c, 0),
                ArgumentKind::FilesAndDirectories,
                "{c}"
            );
        }
        for c in ["echo", "printf", "kill", "which", "type"] {
            assert_eq!(argument_kind(c, 0), ArgumentKind::Nothing, "{c}");
        }
        assert_eq!(
            argument_kind("unknowncmd", 0),
            ArgumentKind::FilesAndDirectories
        );
    }
    #[test]
    fn options_do_not_count_as_positional_words() {
        assert_eq!(positional_index(&["-n", "5"]), 1);
        assert_eq!(positional_index(&["--color=auto"]), 0);
        assert_eq!(positional_index(&[]), 0);
    }
}
