use crate::environment::EnvironmentService;
use crate::path::PathService;

pub(crate) struct ShellServices {
    pub environment: EnvironmentService,
    pub path: PathService,
}

impl ShellServices {
    pub(crate) fn from_process() -> Self {
        let environment = EnvironmentService::from_current();
        let path = PathService::from_environment(environment.get("PATH"));
        Self { environment, path }
    }

    pub(crate) fn sync_path_environment(&mut self) -> Result<(), String> {
        let value = self.path.to_environment()?;
        self.environment.set_os("PATH", value);
        Ok(())
    }

    pub(crate) fn reload_path_from_environment(&mut self) {
        self.path = PathService::from_environment(self.environment.get("PATH"));
    }
}
