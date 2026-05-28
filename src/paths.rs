use std::path::{Path, PathBuf};

use crate::agent::AgentKind;

/// Agent-scoped artifact directory under `<target>/.middleton/<agent>/`.
#[derive(Debug, Clone)]
pub struct ArtifactPaths {
    pub target: PathBuf,
    pub agent: AgentKind,
    pub dir: PathBuf,
    pub rel_prefix: String,
}

impl ArtifactPaths {
    pub fn new(target: &Path, agent: AgentKind) -> Self {
        let rel_prefix = format!(".middleton/{}", agent.label());
        Self {
            target: target.to_path_buf(),
            agent,
            dir: target.join(&rel_prefix),
            rel_prefix,
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
