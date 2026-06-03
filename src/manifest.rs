use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::ArtifactPaths;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionManifest {
    #[serde(flatten)]
    phases: BTreeMap<String, String>,
}

impl SessionManifest {
    pub fn load_or_default(artifacts: &ArtifactPaths) -> Result<Self> {
        let path = artifacts.join("sessions.json");
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

    pub fn save(&self, artifacts: &ArtifactPaths) -> Result<()> {
        artifacts
            .ensure_dir()
            .with_context(|| format!("create artifact directory {}", artifacts.dir.display()))?;
        let path = artifacts.join("sessions.json");
        let contents = serde_json::to_string_pretty(self).context("serialize session manifest")?;
        std::fs::write(&path, contents).with_context(|| format!("write {}", path.display()))
    }
}
