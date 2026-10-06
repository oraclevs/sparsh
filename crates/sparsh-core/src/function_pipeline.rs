use std::ffi::OsString;
use std::path::Path;

use spar_command::{CommandPlan, Join, PipelinePlan, ShellPlan, Step};

pub(crate) const CAPTURED_OUTPUT_PROGRAM: &str = "\0sparsh-captured-output";

#[derive(Debug)]
pub(crate) struct FunctionOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: i32,
}

pub(crate) fn shell_result_output(value: spar::InteractiveEvalResult) -> Result<FunctionOutput, ShellError> {
    let spar::InteractiveEvalResult::Value(spar::ConfigValue::Result(result)) = value else {
        return Err(ShellError::Process { message: "ShellResult function did not return ok or err".into(), status: 2 });
    };
    match result {
        Ok(value) => {
            let value = *value;
            let stdout = match value {
                spar::ConfigValue::Str(text) => line_bytes(text),
                other => {
                    let runtime = spar::Value::from_config(other);
                    let bytes = spar::StructuredFormatRegistry::builtin().encode_values("json", &[runtime])
                        .map_err(|error| ShellError::Process { message: format!("cannot encode ShellResult value: {error}"), status: 2 })?;
                    line_bytes(String::from_utf8(bytes).map_err(|error| ShellError::Process { message: error.to_string(), status: 2 })?)
                }
            };
            Ok(FunctionOutput { stdout, stderr: Vec::new(), status: 0 })
        }
        Err(error) => Ok(FunctionOutput {
            stdout: Vec::new(),
            stderr: line_bytes(spar::Value::from_config(*error).render_display()),
            status: 1,
        }),
    }
}

fn line_bytes(mut value: String) -> Vec<u8> {
    if !value.ends_with('\n') { value.push('\n'); }
    value.into_bytes()
}

pub(crate) const DECODER_PROGRAM: &str = "\0sparsh-decoder";

#[derive(Debug)]
pub(crate) struct ComposedFunctionPipeline {
    pub plan: ShellPlan,
    pub captured_outputs: Vec<FunctionOutput>,
}

use crate::dispatch::is_explicit_call;
use crate::session::ShellError;

/// `f(args) &` or `f(args) & disown`: the call text and whether to disown.
pub(crate) fn split_background_call(input: &str) -> Option<(&str, bool)> {
    let input = input.trim();
    let input = input.strip_suffix(';').unwrap_or(input).trim_end();
    let (head, disown) = match input.strip_suffix("disown") {
        Some(rest) if rest.trim_end().ends_with('&') => (rest.trim_end(), true),
        _ => (input, false),
    };
    let call = head.strip_suffix('&')?.trim_end();
    if call.ends_with('&') || !is_explicit_call(call) || split_top_level_pipeline(call).len() > 1 {
        return None;
    }
    Some((call, disown))
}

pub(crate) fn mark_background(plan: &mut ShellPlan) {
    match plan.steps.last_mut() {
        Some((_, Step::Command(command))) => command.background = true,
        Some((_, Step::Pipeline(pipeline))) => {
            if let Some(last) = pipeline.commands.last_mut() {
                last.background = true;
            }
        }
        None => {}
    }
}

pub(crate) fn compose_function_pipeline(
    source: &str,
    spar: &spar::Session,
    cwd: &Path,
    environment: &[(OsString, OsString)],
    last_status: i32,
) -> Result<Option<ComposedFunctionPipeline>, ShellError> {
    let stages = split_top_level_pipeline(source);
    if stages.len() < 2 || !stages.iter().any(|stage| is_explicit_call(stage.trim())) {
        return Ok(None);
    }

    let mut commands = Vec::new();
    let mut captured_outputs = Vec::new();
    for stage in stages {
        let stage = stage.trim();
        if is_explicit_call(stage) {
            let name = stage.split_once('(').map_or(stage, |(name, _)| name).trim();
            let shell_result = matches!(spar.function_return_type(name),
                Some(spar::ast::SparType::Applied { name, arguments }) if name == "ShellResult" && arguments.len() == 2);
            if shell_result {
                let value = spar.eval_transient_with_context(stage, cwd, environment)
                    .map_err(|errors| ShellError::from_spar(errors, stage))?;
                captured_outputs.push(shell_result_output(value)?);
                commands.push(virtual_command(CAPTURED_OUTPUT_PROGRAM, Vec::new()));
                continue;
            }
            let (value, captured) = spar
                .eval_transient_capture_stdout_with_context(stage, cwd, environment)
                .map_err(|errors| ShellError::from_spar(errors, stage))?;
            let spar::InteractiveEvalResult::Value(spar::ConfigValue::Shell(plan)) = value else {
                return Err(ShellError::Process {
                    message: "pipeline stage must evaluate to shell".into(),
                    status: 2,
                });
            };
            commands.extend(flatten_single_pipeline(plan)?);
            if !captured.is_empty() {
                captured_outputs.push(FunctionOutput { stdout: captured, stderr: Vec::new(), status: 0 });
                commands.push(virtual_command(CAPTURED_OUTPUT_PROGRAM, Vec::new()));
            }
        } else if let Some(format) = stage.strip_prefix("from ") {
            let format = format.trim();
            if format.is_empty() || format.contains(char::is_whitespace) {
                return Err(ShellError::Process {
                    message: "from expects one format name".into(),
                    status: 2,
                });
            }
            commands.push(virtual_command(DECODER_PROGRAM, vec![format.to_string()]));
        } else {
            let plan = spar
                .eval_shell_plan_with_context(stage, cwd, environment, Some(last_status))
                .map_err(|errors| ShellError::from_spar(errors, stage))?;
            commands.extend(flatten_single_pipeline(plan)?);
        }
    }
    if let Some(last) = commands.last_mut() {
        last.background = false;
    }
    Ok(Some(ComposedFunctionPipeline {
        plan: ShellPlan {
            steps: vec![(Join::Always, Step::Pipeline(PipelinePlan { commands }))],
        },
        captured_outputs,
    }))
}

fn virtual_command(program: &str, args: Vec<String>) -> CommandPlan {
    CommandPlan {
        program: program.to_string(),
        args,
        env: Vec::new(),
        cwd: None,
        stdin: None,
        stdout: None,
        stderr: None,
        redirections: Vec::new(),
        background: false,
        glob_args: Vec::new(),
    }
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
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' && !single {
            escaped = true;
            continue;
        }
        if byte == b'\'' && !double {
            single = !single;
            continue;
        }
        if byte == b'"' && !single {
            double = !double;
            continue;
        }
        if single || double {
            continue;
        }
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
        assert_eq!(
            split_top_level_pipeline("emit(name: \"a|b\") | grep a || echo no"),
            ["emit(name: \"a|b\") ", " grep a || echo no"]
        );
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
            0,
        )
        .unwrap()
        .expect("explicit Spar call should trigger mixed-pipeline composition");

        let [(join, Step::Pipeline(pipeline))] = plan.plan.steps.as_slice() else {
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
            0,
        )
        .unwrap_err();

        assert_eq!(error.status(), 2);
        assert!(error
            .to_string()
            .contains("pipeline stage must evaluate to shell"));
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

        let error =
            compose_function_pipeline("emit() | cat", &session, Path::new("/"), &environment, 0)
                .unwrap_err();

        assert_eq!(error.status(), 2);
        assert!(error
            .to_string()
            .contains("must contain exactly one command or pipeline"));
    }
}
