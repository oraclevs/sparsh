use spar_command::{CommandPlan, PipelinePlan, Redirection, ShellPlan, Step, WorkingDirectory};

pub fn render_value(value: &spar::ConfigValue) -> String {
    match value {
        spar::ConfigValue::Str(value) => quote(value),
        spar::ConfigValue::Int(value) => value.to_string(),
        spar::ConfigValue::Float(value) => value.to_string(),
        spar::ConfigValue::Bool(value) => value.to_string(),
        spar::ConfigValue::List(values) => format!(
            "[{}]",
            values
                .iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        spar::ConfigValue::Section(fields) => {
            let mut fields = fields.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(name, _)| *name);
            let body = fields
                .into_iter()
                .map(|(name, value)| format!("{name}: {};", render_value(value)))
                .collect::<Vec<_>>()
                .join(" ");
            format!("{{ {body} }}")
        }
        spar::ConfigValue::Shell(plan) => render_shell_plan(plan),
    }
}

fn render_shell_plan(plan: &ShellPlan) -> String {
    if plan.steps.is_empty() {
        return "shell {}".into();
    }

    let body = plan
        .steps
        .iter()
        .map(|(_, step)| match step {
            Step::Command(command) => render_command(command),
            Step::Pipeline(pipeline) => render_pipeline(pipeline),
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("shell {{ {body}; }}")
}

fn render_pipeline(pipeline: &PipelinePlan) -> String {
    pipeline
        .commands
        .iter()
        .map(render_command)
        .collect::<Vec<_>>()
        .join(" | ")
}

fn render_command(command: &CommandPlan) -> String {
    let mut parts = Vec::new();
    if let Some(WorkingDirectory::Path(path)) = &command.cwd {
        parts.push(format!("[cwd={}]", quote_word(path)));
    }
    parts.extend(
        command
            .env
            .iter()
            .map(|entry| format!("{}={}", entry.key, quote_word(&entry.value))),
    );
    parts.push(quote_word(&command.program));
    parts.extend(command.args.iter().map(|argument| quote_word(argument)));
    if let Some(redirect) = &command.stdin {
        parts.push(render_redirect(0, redirect));
    }
    if let Some(redirect) = &command.stdout {
        parts.push(render_redirect(1, redirect));
    }
    if let Some(redirect) = &command.stderr {
        parts.push(render_redirect(2, redirect));
    }
    parts.join(" ")
}

fn render_redirect(fd: u32, redirect: &Redirection) -> String {
    match redirect {
        Redirection::File { path, mode } => {
            let operator = match (fd, mode) {
                (0, _) => "<",
                (1, spar_command::RedirectMode::Truncate) => ">",
                (1, spar_command::RedirectMode::Append) => ">>",
                (2, spar_command::RedirectMode::Truncate) => "2>",
                (2, spar_command::RedirectMode::Append) => "2>>",
                (_, spar_command::RedirectMode::Truncate) => ">",
                (_, spar_command::RedirectMode::Append) => ">>",
            };
            format!("{operator} {}", quote_word(path))
        }
        Redirection::DuplicateFd(target) => format!("{fd}>&{target}"),
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", escape(value))
}

fn quote_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-._/:".contains(character))
    {
        value.to_string()
    } else {
        quote(value)
    }
}

fn escape(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            '\\' => "\\\\".to_string(),
            '"' => "\\\"".to_string(),
            other => other.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use spar::ConfigValue;
    use spar_command::{
        CommandPlan, Join, RedirectMode, Redirection, ShellPlan, Step, WorkingDirectory,
    };

    #[test]
    fn renders_scalars_as_spar_literals() {
        assert_eq!(
            super::render_value(&ConfigValue::Str("a\n\"b".into())),
            r#""a\n\"b""#
        );
        assert_eq!(super::render_value(&ConfigValue::Int(42)), "42");
        assert_eq!(super::render_value(&ConfigValue::Float(2.5)), "2.5");
        assert_eq!(super::render_value(&ConfigValue::Bool(true)), "true");
    }

    #[test]
    fn renders_lists_recursively() {
        let value = ConfigValue::List(vec![ConfigValue::Str("spar".into()), ConfigValue::Int(2)]);
        assert_eq!(super::render_value(&value), r#"["spar", 2]"#);
    }

    #[test]
    fn renders_sections_in_stable_key_order() {
        let value = ConfigValue::Section(HashMap::from([
            ("z".into(), ConfigValue::Int(2)),
            ("a".into(), ConfigValue::Int(1)),
        ]));
        assert_eq!(super::render_value(&value), "{ a: 1; z: 2; }");
    }

    #[test]
    fn renders_shell_values_as_data_previews() {
        let value = ConfigValue::Shell(ShellPlan { steps: vec![] });
        assert_eq!(super::render_value(&value), "shell {}");
    }

    #[test]
    fn shell_preview_uses_structured_arguments_and_redirects() {
        let value = ConfigValue::Shell(ShellPlan {
            steps: vec![(
                Join::Always,
                Step::Command(CommandPlan {
                    program: "printf".into(),
                    args: vec!["hello world".into()],
                    env: vec![],
                    cwd: Some(WorkingDirectory::Path("/tmp/my project".into())),
                    stdin: None,
                    stdout: Some(Redirection::File {
                        path: "output file".into(),
                        mode: RedirectMode::Append,
                    }),
                    stderr: Some(Redirection::DuplicateFd(1)),
                }),
            )],
        });

        assert_eq!(
            super::render_value(&value),
            r#"shell { [cwd="/tmp/my project"] printf "hello world" >> "output file" 2>&1; }"#
        );
    }
}
