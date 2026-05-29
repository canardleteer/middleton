use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::info;

use crate::agent::{AgentKind, ReviewProfile};
use crate::paths::ArtifactPaths;

pub const LOG_FILENAME: &str = "actions.log";

/// Append-only audit log of middleton run metadata and user-confirmed actions.
#[derive(Clone)]
pub struct RunLog {
    inner: Arc<Mutex<BufWriter<File>>>,
    path: PathBuf,
}

/// Snapshot of CLI options recorded at run start.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub input: String,
    pub target: PathBuf,
    pub agent: AgentKind,
    pub profile: ReviewProfile,
    pub model: String,
    pub hostname: String,
    pub opencode_bin: String,
    pub claude_bin: String,
    pub codex_bin: String,
    pub skip_pdf: bool,
    pub note_present: bool,
}

/// A permission, tool use, command, or prompt answer middleton approved on the user's behalf.
#[derive(Debug, Clone)]
pub struct ConfirmedAction<'a> {
    pub agent: &'static str,
    pub phase: &'a str,
    pub step: &'static str,
    pub kind: &'static str,
    pub detail: String,
}

impl RunLog {
    pub fn open(artifacts: &ArtifactPaths) -> Result<Self> {
        artifacts
            .ensure_dir()
            .with_context(|| format!("create {}", artifacts.dir.display()))?;
        let path = artifacts.join(LOG_FILENAME);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open append-only log {}", path.display()))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(BufWriter::new(file))),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record_run_start(&self, options: &RunOptions) -> Result<()> {
        let target = canonical_display(&options.target);
        let started_at = Utc::now();
        let block = format!(
            "\
================================================================================\n\
RUN {started_at}\n\
target={target}\n\
input={input}\n\
agent={agent}\n\
profile={profile}\n\
model={model}\n\
hostname={hostname}\n\
opencode_bin={opencode_bin}\n\
claude_bin={claude_bin}\n\
codex_bin={codex_bin}\n\
skip_pdf={skip_pdf}\n\
note_present={note_present}\n\
================================================================================\n",
            input = options.input,
            agent = options.agent.label(),
            profile = options.profile.label(),
            model = options.model,
            hostname = options.hostname,
            opencode_bin = options.opencode_bin,
            claude_bin = options.claude_bin,
            codex_bin = options.codex_bin,
            skip_pdf = options.skip_pdf,
            note_present = options.note_present,
        );
        self.write_block(&block)?;

        info!(
            log = %self.path.display(),
            started_at = %started_at.to_rfc3339(),
            target = %target,
            agent = options.agent.label(),
            profile = options.profile.label(),
            model = %options.model,
            "middleton run logged"
        );
        Ok(())
    }

    pub fn record_confirmed(&self, action: ConfirmedAction<'_>) -> Result<()> {
        let timestamp = Utc::now();
        let line = format!(
            "{timestamp} CONFIRMED agent={} phase={} step={} kind={} detail={}\n",
            action.agent, action.phase, action.step, action.kind, action.detail,
        );
        self.write_block(&line)?;

        info!(
            log = %self.path.display(),
            agent = action.agent,
            phase = action.phase,
            step = action.step,
            kind = action.kind,
            detail = %action.detail,
            "middleton confirmed action for user"
        );
        Ok(())
    }

    fn write_block(&self, text: &str) -> Result<()> {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .write_all(text.as_bytes())
            .with_context(|| format!("append to {}", self.path.display()))?;
        guard
            .flush()
            .with_context(|| format!("flush {}", self.path.display()))?;
        Ok(())
    }
}

fn canonical_display(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::paths::ArtifactPaths;

    fn temp_artifacts() -> (tempfile::TempDir, ArtifactPaths) {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("repo");
        fs::create_dir_all(&target).unwrap();
        (dir, ArtifactPaths::new(&target, AgentKind::OpenCode))
    }

    #[test]
    fn appends_run_header_and_confirmed_lines() {
        let (_dir, artifacts) = temp_artifacts();
        let log = RunLog::open(&artifacts).unwrap();
        log.record_run_start(&RunOptions {
            input: "./repo".to_string(),
            target: artifacts.target.clone(),
            agent: AgentKind::OpenCode,
            profile: ReviewProfile::Documents,
            model: "kimi".to_string(),
            hostname: "127.0.0.1".to_string(),
            opencode_bin: "opencode".to_string(),
            claude_bin: "claude".to_string(),
            codex_bin: "codex".to_string(),
            skip_pdf: false,
            note_present: true,
        })
        .unwrap();
        log.record_confirmed(ConfirmedAction {
            agent: "opencode",
            phase: "intent",
            step: "plan",
            kind: "permission",
            detail: "bash.execute git log".to_string(),
        })
        .unwrap();

        let contents = fs::read_to_string(log.path()).unwrap();
        assert!(contents.contains("RUN "));
        assert!(contents.contains("profile=documents"));
        assert!(contents.contains("CONFIRMED"));
        assert!(contents.contains("git log"));
    }

    #[test]
    fn log_file_lives_under_middleton_agent_dir() {
        let (_dir, artifacts) = temp_artifacts();
        let log = RunLog::open(&artifacts).unwrap();
        assert!(log.path().ends_with(".middleton/opencode/actions.log"));
    }
}
