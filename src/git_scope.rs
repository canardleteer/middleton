use std::path::Path;

use git2::Repository;
use tracing::warn;

/// Returns a warning message when `.middleton/` is not gitignored in a git repository.
pub fn middleton_gitignore_warning(target: &Path) -> Option<String> {
    let repo = Repository::open(target).ok()?;
    if middleton_is_gitignored(&repo) {
        return None;
    }

    Some(format!(
        "Middleton wrote artifacts under `{MIDDLETON_IGNORE_PATH}` inside a git repository, \
         but that path is not listed in `.gitignore`. Consider adding `{MIDDLETON_IGNORE_PATH}` \
         to avoid committing analysis output."
    ))
}

fn middleton_is_gitignored(repo: &Repository) -> bool {
    [
        MIDDLETON_IGNORE_PATH,
        ".middleton/run-artifact.md",
        ".middleton/opencode/kimi-k2-5/20250602-1430/DEPTH.md",
    ]
    .into_iter()
    .any(|path| repo.is_path_ignored(path).unwrap_or(false))
}

pub fn warn_if_middleton_not_gitignored(target: &Path) {
    if let Some(message) = middleton_gitignore_warning(target) {
        warn!("{message}");
    }
}

const MIDDLETON_IGNORE_PATH: &str = ".middleton";

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn init_git_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(dir)
            .output()
            .expect("git config name");
    }

    #[test]
    fn warns_when_middleton_not_gitignored() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        fs::write(dir.path().join("README.md"), "hello").unwrap();
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        assert!(middleton_gitignore_warning(dir.path()).is_some());
    }

    #[test]
    fn silent_when_middleton_gitignored() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        fs::write(dir.path().join(".gitignore"), ".middleton/\n").unwrap();
        fs::write(dir.path().join("README.md"), "hello").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        assert!(middleton_gitignore_warning(dir.path()).is_none());
    }
}
