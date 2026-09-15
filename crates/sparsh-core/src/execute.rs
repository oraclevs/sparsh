use spar_command::{Join, ShellPlan, Step};

use crate::builtin::{BuiltinContext, BuiltinRegistry};
use crate::session::{ShellError, ShellResult};

pub(crate) fn eligible_builtin<'a>(
    plan: &'a ShellPlan,
    registry: &BuiltinRegistry,
) -> Option<(&'a str, &'a [String])> {
    let [(Join::Always, Step::Command(command))] = plan.steps.as_slice() else {
        return None;
    };
    registry
        .find(&command.program)
        .map(|_| (command.program.as_str(), command.args.as_slice()))
}

pub(crate) fn execute_plan(
    plan: &ShellPlan,
    registry: &BuiltinRegistry,
    last_status: i32,
) -> Result<ShellResult, ShellError> {
    if let Some((name, arguments)) = eligible_builtin(plan, registry) {
        let mut context = BuiltinContext {
            last_status,
            requested_exit: None,
        };
        let output = registry
            .execute(name, arguments, &mut context)
            .map_err(ShellError::Builtin)?;
        if let Some(status) = context.requested_exit {
            return Ok(ShellResult::Exit(status));
        }
        return Ok(ShellResult::Builtin(output));
    }

    spar::execute_shell_plan(plan)
        .map(ShellResult::Process)
        .map_err(|error| ShellError::Process {
            status: if error.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                1
            },
            message: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use spar_command::{CommandPlan, Join, PipelinePlan, ShellPlan, Step};

    use crate::builtin::BuiltinRegistry;

    fn command(program: &str) -> CommandPlan {
        CommandPlan {
            program: program.into(),
            args: vec![],
            env: vec![],
            cwd: None,
            stdin: None,
            stdout: None,
            stderr: None,
        }
    }

    #[test]
    fn only_one_always_joined_command_is_builtin_eligible() {
        let registry = BuiltinRegistry::new();
        let single = ShellPlan {
            steps: vec![(Join::Always, Step::Command(command("pwd")))],
        };
        let pipeline = ShellPlan {
            steps: vec![(
                Join::Always,
                Step::Pipeline(PipelinePlan {
                    commands: vec![command("pwd"), command("cat")],
                }),
            )],
        };

        assert!(super::eligible_builtin(&single, &registry).is_some());
        assert!(super::eligible_builtin(&pipeline, &registry).is_none());
    }
}
