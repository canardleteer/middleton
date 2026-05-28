use std::path::Path;

use anyhow::{Context, Result, bail};
use claudecode::{Client, Model, PermissionMode, SessionConfig};
use tracing::{info, warn};

use crate::output::{
    log_missing_outputs, log_outputs_ready, missing_outputs, nudge_prompt, settle_outputs,
    verify_outputs,
};

const READ_ONLY_TOOLS: &[&str] = &["Bash", "WebFetch", "WebSearch"];
const MAX_BUILD_ATTEMPTS: u32 = 3;

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
    let plan_config = build_config(
        plan_prompt,
        target,
        model,
        PermissionMode::Plan,
        None,
        phase,
        "plan",
    )?;

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

    ensure_build_outputs(
        client,
        phase,
        build_prompt,
        model,
        target,
        &session_id,
        expected_outputs,
    )
    .await?;

    info!(
        phase,
        session_id = %session_id,
        outputs = expected_outputs.len(),
        "phase complete"
    );

    Ok(session_id)
}

async fn ensure_build_outputs(
    client: &Client,
    phase: &str,
    build_prompt: &str,
    model: Model,
    target: &Path,
    session_id: &str,
    expected_outputs: &[&Path],
) -> Result<()> {
    let mut prompt = build_prompt.to_string();

    for attempt in 1..=MAX_BUILD_ATTEMPTS {
        info!(phase, session_id = %session_id, attempt, "starting build step");
        let build_config = build_config(
            &prompt,
            target,
            model,
            PermissionMode::AcceptEdits,
            Some(session_id),
            phase,
            "build",
        )?;

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

        let _ = settle_outputs(expected_outputs).await;

        let missing = missing_outputs(expected_outputs);
        if missing.is_empty() {
            log_outputs_ready(phase, attempt);
            return Ok(());
        }

        log_missing_outputs(phase, attempt, MAX_BUILD_ATTEMPTS, &missing);

        if attempt == MAX_BUILD_ATTEMPTS {
            return verify_outputs(expected_outputs, phase);
        }

        prompt = nudge_prompt(&missing);
    }

    verify_outputs(expected_outputs, phase)
}

fn build_config(
    prompt: &str,
    target: &Path,
    model: Model,
    permission_mode: PermissionMode,
    session_id: Option<&str>,
    phase: &str,
    step: &str,
) -> Result<SessionConfig> {
    let mut builder = SessionConfig::builder(prompt)
        .working_dir(target)
        .model(model)
        .permission_mode(permission_mode)
        .disallowed_tools(
            READ_ONLY_TOOLS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
        );

    if let Some(session_id) = session_id {
        builder = builder.resume_session_id(session_id);
    }

    builder
        .build()
        .with_context(|| format!("build claudecode {step} config for {phase}"))
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
