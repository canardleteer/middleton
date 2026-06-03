use std::path::{Path, PathBuf};

use chrono::Local;

use crate::agent::AgentKind;

pub const MIDDLETON_BASE: &str = ".middleton";

/// Run-scoped artifact directory under `<target>/.middleton/<agent>/<model-slug>/<timestamp>/`.
#[derive(Debug, Clone)]
pub struct ArtifactPaths {
    pub target: PathBuf,
    #[allow(dead_code)]
    pub agent: AgentKind,
    #[allow(dead_code)]
    pub base: PathBuf,
    pub dir: PathBuf,
    pub rel_prefix: String,
    pub base_display: String,
}

impl ArtifactPaths {
    pub fn new(target: &Path, agent: AgentKind, model: &str) -> Self {
        let timestamp = Local::now().format("%Y%m%d-%H%M").to_string();
        Self::with_timestamp(target, agent, model, &timestamp)
    }

    pub fn with_timestamp(
        target: &Path,
        agent: AgentKind,
        model: &str,
        run_timestamp: &str,
    ) -> Self {
        let model_slug = slugify(model);
        let rel_prefix = format!(
            "{MIDDLETON_BASE}/{}/{model_slug}/{run_timestamp}",
            agent.label()
        );
        let base = target.join(MIDDLETON_BASE);
        let dir = target.join(&rel_prefix);
        Self {
            target: target.to_path_buf(),
            agent,
            base,
            dir,
            rel_prefix,
            base_display: MIDDLETON_BASE.to_string(),
        }
    }

    pub fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn rel(&self, name: &str) -> String {
        format!("{}/{}", self.rel_prefix, name)
    }
}

/// Returns true when `path` refers to the current run's artifact directory.
pub fn allows_write_path(path: &str, rel_prefix: &str, run_dir: &Path) -> bool {
    let normalized = path.replace('\\', "/");
    let prefix = format!("{rel_prefix}/");
    if normalized.contains(&prefix) || normalized.ends_with(rel_prefix) {
        return true;
    }
    path_contains_dir(path, run_dir)
}

fn path_contains_dir(path: &str, dir: &Path) -> bool {
    let Ok(canonical_dir) = dir.canonicalize() else {
        return false;
    };
    let candidate = Path::new(path);
    let Ok(canonical_path) = candidate.canonicalize() else {
        return false;
    };
    canonical_path.starts_with(&canonical_dir)
}

/// Lowercase slug for filesystem paths: non-alphanumeric runs become `-`, repeats collapse.
pub fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in input.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;

    #[test]
    fn slugify_normalizes_model_ids() {
        assert_eq!(slugify("kimi-k2.5"), "kimi-k2-5");
        assert_eq!(slugify("sonnet"), "sonnet");
        assert_eq!(slugify("  GPT-5  "), "gpt-5");
    }

    #[test]
    fn run_dir_uses_timestamped_layout() {
        let paths = ArtifactPaths::with_timestamp(
            Path::new("/repo"),
            AgentKind::OpenCode,
            "kimi-k2.5",
            "20250602-1430",
        );
        assert_eq!(
            paths.rel_prefix,
            ".middleton/opencode/kimi-k2-5/20250602-1430"
        );
        assert_eq!(
            paths.dir,
            PathBuf::from("/repo/.middleton/opencode/kimi-k2-5/20250602-1430")
        );
        assert_eq!(paths.base, PathBuf::from("/repo/.middleton"));
        assert_eq!(paths.base_display, ".middleton");
    }

    #[test]
    fn rel_prefixes_artifact_files() {
        let paths = ArtifactPaths::with_timestamp(
            Path::new("/repo"),
            AgentKind::Codex,
            "gpt-5",
            "20250602-1200",
        );
        assert_eq!(
            paths.rel("DEPTH.md"),
            ".middleton/codex/gpt-5/20250602-1200/DEPTH.md"
        );
    }

    #[test]
    fn allows_write_matches_run_prefix() {
        let paths = ArtifactPaths::with_timestamp(
            Path::new("/repo"),
            AgentKind::ClaudeCode,
            "sonnet",
            "20250602-0900",
        );
        assert!(allows_write_path(
            "/repo/.middleton/claude/sonnet/20250602-0900/DEPTH.md",
            &paths.rel_prefix,
            Path::new(""),
        ));
        assert!(allows_write_path(
            ".middleton/claude/sonnet/20250602-0900/DEPTH.md",
            &paths.rel_prefix,
            Path::new(""),
        ));
        assert!(!allows_write_path(
            ".middleton/claude/sonnet/20250601-0900/DEPTH.md",
            &paths.rel_prefix,
            Path::new(""),
        ));
        assert!(!allows_write_path(
            "/repo/src/main.rs",
            &paths.rel_prefix,
            Path::new(""),
        ));
    }
}
