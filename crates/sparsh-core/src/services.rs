use crate::alias::AliasService;
use crate::directory::DirectoryService;
use crate::environment::EnvironmentService;
use crate::path::PathService;
use crate::resolver::CommandResolver;

pub(crate) struct ShellServices {
    pub aliases: AliasService,
    pub directories: DirectoryService,
    pub environment: EnvironmentService,
    pub path: PathService,
    pub resolver: CommandResolver,
}

impl ShellServices {
    pub(crate) fn from_process() -> Result<Self, String> {
        let mut environment = EnvironmentService::from_current();
        let path = PathService::from_environment(environment.get("PATH"));
        let directories = DirectoryService::new()?;
        environment.set_os("PWD", directories.current().as_os_str());
        Ok(Self {
            aliases: AliasService::new(),
            directories,
            environment,
            path,
            resolver: CommandResolver::new(),
        })
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
