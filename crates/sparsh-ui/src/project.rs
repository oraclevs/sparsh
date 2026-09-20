use std::path::Path;

use sparsh_core::ShellUiSnapshot;

use crate::theme::SemanticRole;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectKind {
    Python,
    Rust,
    Flutter,
    Dart,
    Node,
    Go,
}

impl ProjectKind {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Python => "",
            Self::Rust => "",
            Self::Flutter => "",
            Self::Dart => "",
            Self::Node => "",
            Self::Go => "",
        }
    }

    pub(crate) fn role(self) -> SemanticRole {
        match self {
            Self::Python => SemanticRole::ProjectPython,
            Self::Rust => SemanticRole::ProjectRust,
            Self::Flutter => SemanticRole::ProjectFlutter,
            Self::Dart => SemanticRole::ProjectDart,
            Self::Node => SemanticRole::ProjectNode,
            Self::Go => SemanticRole::ProjectGo,
        }
    }
}

pub fn detect_projects(cwd: &Path) -> Vec<ProjectKind> {
    for directory in cwd.ancestors() {
        let projects = projects_at(directory);
        if !projects.is_empty() {
            return projects;
        }
    }
    Vec::new()
}

pub fn active_python_environment(snapshot: &ShellUiSnapshot) -> Option<String> {
    if let Some(value) = snapshot.environment_value("VIRTUAL_ENV") {
        let path = Path::new(value);
        return Some(
            path.file_name()
                .filter(|name| !name.is_empty())
                .unwrap_or(value)
                .to_string_lossy()
                .into_owned(),
        );
    }

    snapshot
        .environment_value("CONDA_DEFAULT_ENV")
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
}

fn projects_at(directory: &Path) -> Vec<ProjectKind> {
    let mut projects = Vec::new();

    if has_any(
        directory,
        &[
            "pyproject.toml",
            "requirements.txt",
            "setup.py",
            "setup.cfg",
            "Pipfile",
            "poetry.lock",
            "uv.lock",
        ],
    ) {
        projects.push(ProjectKind::Python);
    }

    if directory.join("Cargo.toml").is_file() {
        projects.push(ProjectKind::Rust);
    }

    let pubspec = directory.join("pubspec.yaml");
    if pubspec.is_file() {
        if is_flutter_project(directory, &pubspec) {
            projects.push(ProjectKind::Flutter);
        } else {
            projects.push(ProjectKind::Dart);
        }
    }

    if directory.join("package.json").is_file() {
        projects.push(ProjectKind::Node);
    }

    if directory.join("go.mod").is_file() {
        projects.push(ProjectKind::Go);
    }

    projects
}

fn has_any(directory: &Path, names: &[&str]) -> bool {
    names.iter().any(|name| directory.join(name).is_file())
}

fn is_flutter_project(directory: &Path, pubspec: &Path) -> bool {
    if directory.join(".metadata").is_file() {
        return true;
    }

    std::fs::read_to_string(pubspec).is_ok_and(|source| {
        source.lines().any(|line| {
            let line = line.trim();
            line == "flutter:" || line == "sdk: flutter" || line.ends_with("sdk: flutter")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{detect_projects, ProjectKind};

    #[test]
    fn detects_nearest_project_root_from_a_nested_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        let nested = root.path().join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(detect_projects(&nested), vec![ProjectKind::Rust]);
    }

    #[test]
    fn detects_python_and_node_in_a_polyglot_project() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("pyproject.toml"), "[project]\nname = \"demo\"\n").unwrap();
        std::fs::write(root.path().join("package.json"), "{}\n").unwrap();

        assert_eq!(
            detect_projects(root.path()),
            vec![ProjectKind::Python, ProjectKind::Node]
        );
    }

    #[cfg(unix)]
    #[test]
    fn sourced_python_virtualenv_name_is_visible_to_the_prompt_layer() {
        use sparsh_core::ShellSession;

        let root = tempfile::tempdir().unwrap();
        let venv = root.path().join(".venv");
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        let activate = venv.join("bin/activate");
        std::fs::write(
            &activate,
            format!("export VIRTUAL_ENV='{}'\n", venv.display()),
        )
        .unwrap();

        let mut session = ShellSession::new();
        session
            .submit(&format!("source {}", activate.display()))
            .unwrap();

        assert_eq!(
            super::active_python_environment(&session.ui_snapshot()).as_deref(),
            Some(".venv")
        );
    }

    #[test]
    fn flutter_pubspec_uses_flutter_icon_instead_of_plain_dart() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("pubspec.yaml"),
            "dependencies:\n  flutter:\n    sdk: flutter\n",
        )
        .unwrap();

        assert_eq!(detect_projects(root.path()), vec![ProjectKind::Flutter]);
    }
}
