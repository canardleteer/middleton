use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::agent::AgentKind;
use crate::paths::ArtifactPaths;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionManifest {
    #[serde(flatten)]
    phases: BTreeMap<String, String>,
}

impl SessionManifest {
    pub fn load_or_default(target: &Path, agent: AgentKind) -> Result<Self> {
        let path = manifest_path(target, agent);
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&contents).with_context(|| format!("parse {}", path.display()))
    }

    pub fn set(&mut self, phase: &str, session_id: String) {
        self.phases.insert(phase.to_string(), session_id);
    }

    pub fn save(&self, target: &Path, agent: AgentKind) -> Result<()> {
        let path = manifest_path(target, agent);
        let contents = serde_json::to_string_pretty(self).context("serialize session manifest")?;
        std::fs::write(&path, contents).with_context(|| format!("write {}", path.display()))
    }
}

fn manifest_path(target: &Path, agent: AgentKind) -> std::path::PathBuf {
    ArtifactPaths::new(target, agent).join("sessions.json")
}
