use std::path::Path;

use anyhow::{Context, Result, bail};
use claudecode::{Client, Model, PermissionMode, SessionConfig};
use tracing::{info, warn};

const READ_ONLY_TOOLS: &[&str] = &["Bash", "WebFetch", "WebSearch"];

pub struct ClaudeCodeRuntime {
    pub client: Client,
}

pub async fn start_runtime(claude_bin: &str) -> Result<ClaudeCodeRuntime> {
    let client = if claude_bin == "claude" {
        Client::new().await.context("create Claude Code client")?
    } else {
        Client::with_path(claude_bin)
            .await
            .with_context(|| format!("create Claude Code client at {claude_bin}"))?
    };

    Ok(ClaudeCodeRuntime { client })
}

pub fn parse_model(model: &str) -> Model {
    match model.trim().to_ascii_lowercase().as_str() {
        "opus" => Model::Opus,
        "haiku" => Model::Haiku,
        "sonnet" => Model::Sonnet,
        other => {
            warn!(
                model = other,
                "using Claude Code sonnet model; claudecode accepts sonnet, opus, or haiku"
            );
            Model::Sonnet
        }
    }
}

pub fn model_label(model: Model) -> &'static str {
    match model {
        Model::Sonnet => "sonnet",
        Model::Opus => "opus",
        Model::Haiku => "haiku",
    }
}

pub async fn run_plan_build_phase(
    client: &Client,
    phase: &str,
    plan_prompt: &str,
    build_prompt: &str,
    model: Model,
    target: &Path,
    expected_outputs: &[&Path],
) -> Result<String> {
    info!(phase, agent = "claudecode", "starting plan step");
    let plan_config = SessionConfig::builder(plan_prompt)
        .working_dir(target)
        .model(model)
        .permission_mode(PermissionMode::Plan)
        .disallowed_tools(
            READ_ONLY_TOOLS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
        )
        .build()
        .with_context(|| format!("build claudecode plan config for {phase}"))?;

    let plan_result = client
        .launch_and_wait(plan_config)
        .await
        .with_context(|| format!("run claudecode plan step for {phase}"))?;

    if plan_result.is_error {
        bail!(
            "claudecode plan step failed for {phase}: {}",
            plan_result
                .error
                .or(plan_result.content)
                .unwrap_or_else(|| "unknown error".to_string())
        );
    }

    let session_id = plan_result
        .session_id
        .context("claudecode plan step did not return a session id")?;

    info!(phase, session_id = %session_id, "plan step complete");

    info!(phase, session_id = %session_id, "starting build step");
    let build_config = SessionConfig::builder(build_prompt)
        .working_dir(target)
        .model(model)
        .resume_session_id(&session_id)
        .permission_mode(PermissionMode::AcceptEdits)
        .disallowed_tools(
            READ_ONLY_TOOLS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
        )
        .build()
        .with_context(|| format!("build claudecode build config for {phase}"))?;

    let build_result = client
        .launch_and_wait(build_config)
        .await
        .with_context(|| format!("run claudecode build step for {phase}"))?;

    if build_result.is_error {
        bail!(
            "claudecode build step failed for {phase}: {}",
            build_result
                .error
                .or(build_result.content)
                .unwrap_or_else(|| "unknown error".to_string())
        );
    }

    for output in expected_outputs {
        verify_output(output, phase)?;
    }

    info!(
        phase,
        session_id = %session_id,
        outputs = expected_outputs.len(),
        "phase complete"
    );

    Ok(session_id)
}

fn verify_output(path: &Path, phase: &str) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("expected output for {phase} at {}", path.display()))?;

    if metadata.len() == 0 {
        bail!("output for {phase} is empty: {}", path.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_accepts_known_values() {
        assert_eq!(parse_model("opus"), Model::Opus);
        assert_eq!(parse_model("Haiku"), Model::Haiku);
        assert_eq!(parse_model("sonnet"), Model::Sonnet);
        assert_eq!(parse_model("kimi-k2.5"), Model::Sonnet);
    }
}
