use std::ffi::OsString;
use std::path::Path;

use spar_command::{Join, PipelinePlan, ShellPlan, Step};

use crate::dispatch::is_explicit_call;
use crate::session::ShellError;

pub(crate) fn compose_function_pipeline(
    source: &str,
    spar: &spar::Session,
    cwd: &Path,
    environment: &[(OsString, OsString)],
) -> Result<Option<ShellPlan>, ShellError> {
    let stages = split_top_level_pipeline(source);
    if stages.len() < 2 || !stages.iter().any(|stage| is_explicit_call(stage.trim())) {
        return Ok(None);
    }

    let mut commands = Vec::new();
    for stage in stages {
        let stage = stage.trim();
        if is_explicit_call(stage) {
            let value = spar
                .eval_transient_with_context(stage, cwd, environment)
                .map_err(ShellError::Spar)?;
            let spar::InteractiveEvalResult::Value(spar::ConfigValue::Shell(plan)) = value else {
                return Err(ShellError::Process {
                    message: "pipeline stage must evaluate to shell".into(),
                    status: 2,
                });
            };
            commands.extend(flatten_single_pipeline(plan)?);
        } else {
            let plan = spar
                .eval_shell_plan_with_context(stage, cwd, environment)
                .map_err(ShellError::Spar)?;
            commands.extend(flatten_single_pipeline(plan)?);
        }
    }
    if let Some(last) = commands.last_mut() {
        last.background = false;
    }
    Ok(Some(ShellPlan {
        steps: vec![(Join::Always, Step::Pipeline(PipelinePlan { commands }))],
    }))
}

fn flatten_single_pipeline(plan: ShellPlan) -> Result<Vec<spar_command::CommandPlan>, ShellError> {
    if plan.steps.len() != 1 {
        return Err(ShellError::Process {
            message: "pipeline function stage must contain exactly one command or pipeline (no &&, ||, or sequential steps)".into(),
            status: 2,
        });
    }
    let (join, step) = plan.steps.into_iter().next().unwrap();
    if join != Join::Always {
        return Err(ShellError::Process {
            message: "conditional shell plans cannot be embedded as a pipeline stage".into(),
            status: 2,
        });
    }
    let commands = match step {
        Step::Command(command) => vec![command],
        Step::Pipeline(pipeline) => pipeline.commands,
    };
    if commands.iter().any(|command| command.background) {
        return Err(ShellError::Process {
            message: "background shell plans cannot be embedded as a pipeline stage".into(),
            status: 2,
        });
    }
    Ok(commands)
}

fn split_top_level_pipeline(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut brace = 0usize;
    let mut single = false;
    let mut double = false;
    let mut escaped = false;

    for (index, byte) in bytes.iter().copied().enumerate() {
        if escaped { escaped = false; continue; }
        if byte == b'\\' && !single { escaped = true; continue; }
        if byte == b'\'' && !double { single = !single; continue; }
        if byte == b'"' && !single { double = !double; continue; }
        if single || double { continue; }
        match byte {
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'[' => bracket += 1,
            b']' => bracket = bracket.saturating_sub(1),
            b'{' => brace += 1,
            b'}' => brace = brace.saturating_sub(1),
            b'|' if paren == 0 && bracket == 0 && brace == 0 => {
                let previous_pipe = index > 0 && bytes[index - 1] == b'|';
                let next_pipe = bytes.get(index + 1) == Some(&b'|');
                if !previous_pipe && !next_pipe {
                    result.push(&source[start..index]);
                    start = index + 1;
                }
            }
            _ => {}
        }
    }
    result.push(&source[start..]);
    result
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use spar_command::Step;

    use super::{compose_function_pipeline, split_top_level_pipeline};

    #[test]
    fn splits_only_top_level_single_pipes() {
        assert_eq!(split_top_level_pipeline("emit(name: \"a|b\") | grep a || echo no"), ["emit(name: \"a|b\") ", " grep a || echo no"]);
    }

    #[test]
    fn named_argument_shell_function_can_be_a_pipeline_stage() {
        let engine = spar::Engine::default();
        let mut session = engine.session();
        session
            .eval(
                r#"function emit(value: str) -> shell {
                    return shell { printf "%s\n" ${value}; };
                };"#,
            )
            .unwrap();
        let environment: Vec<(OsString, OsString)> = Vec::new();

        let plan = compose_function_pipeline(
            r#"emit(value: "alpha") | grep alpha"#,
            &session,
            Path::new("/"),
            &environment,
        )
        .unwrap()
        .expect("explicit Spar call should trigger mixed-pipeline composition");

        let [(join, Step::Pipeline(pipeline))] = plan.steps.as_slice() else {
            panic!("expected one composed pipeline");
        };
        assert_eq!(*join, spar_command::Join::Always);
        assert_eq!(pipeline.commands.len(), 2);
        assert_eq!(pipeline.commands[0].program, "printf");
        assert_eq!(pipeline.commands[1].program, "grep");
    }

    #[test]
    fn non_shell_function_is_rejected_as_a_pipeline_stage() {
        let engine = spar::Engine::default();
        let mut session = engine.session();
        session
            .eval(r#"function value(name: str) -> str { return name; };"#)
            .unwrap();
        let environment: Vec<(OsString, OsString)> = Vec::new();

        let error = compose_function_pipeline(
            r#"value(name: "alpha") | grep alpha"#,
            &session,
            Path::new("/"),
            &environment,
        )
        .unwrap_err();

        assert_eq!(error.status(), 2);
        assert!(error.to_string().contains("pipeline stage must evaluate to shell"));
    }

    #[test]
    fn joined_shell_plan_is_rejected_as_a_single_pipeline_stage() {
        let engine = spar::Engine::default();
        let mut session = engine.session();
        session
            .eval(
                r#"function emit() -> shell {
                    return shell { printf one; printf two; };
                };"#,
            )
            .unwrap();
        let environment: Vec<(OsString, OsString)> = Vec::new();

        let error = compose_function_pipeline(
            "emit() | cat",
            &session,
            Path::new("/"),
            &environment,
        )
        .unwrap_err();

        assert_eq!(error.status(), 2);
        assert!(error
            .to_string()
            .contains("must contain exactly one command or pipeline"));
    }
}
