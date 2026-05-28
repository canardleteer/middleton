use std::path::Path;

use anyhow::{Context, Result, bail};
use claude_codes::{
    AsyncClient, ClaudeCliBuilder, ClaudeOutput, PermissionMode, io::ResultMessage,
};
use tracing::{info, warn};

use crate::output::{
    log_missing_outputs, log_outputs_ready, missing_outputs, nudge_prompt, settle_outputs,
    verify_outputs,
};

const READ_ONLY_TOOLS: &[&str] = &["Bash", "WebFetch", "WebSearch"];
const MAX_BUILD_ATTEMPTS: u32 = 3;

struct StepRequest<'a> {
    claude_bin: &'a str,
    prompt: &'a str,
    target: &'a Path,
    model: &'a str,
    permission_mode: PermissionMode,
    resume_session_id: Option<&'a str>,
    phase: &'a str,
    step: &'a str,
}

pub struct ClaudeCodeRuntime {
    pub claude_bin: String,
}

pub async fn start_runtime(claude_bin: &str) -> Result<ClaudeCodeRuntime> {
    Ok(ClaudeCodeRuntime {
        claude_bin: claude_bin.to_string(),
    })
}

pub fn parse_model(model: &str) -> &'static str {
    match model.trim().to_ascii_lowercase().as_str() {
        "opus" => "opus",
        "haiku" => "haiku",
        "sonnet" => "sonnet",
        other => {
            warn!(
                model = other,
                "using Claude Code sonnet model; claude-codes accepts sonnet, opus, or haiku"
            );
            "sonnet"
        }
    }
}

pub fn model_label(model: &str) -> &str {
    model
}

pub async fn run_plan_build_phase(
    runtime: &ClaudeCodeRuntime,
    phase: &str,
    plan_prompt: &str,
    build_prompt: &str,
    model: &str,
    target: &Path,
    expected_outputs: &[&Path],
) -> Result<String> {
    info!(phase, agent = "claude-codes", "starting plan step");
    let (session_id, _) = run_step(StepRequest {
        claude_bin: &runtime.claude_bin,
        prompt: plan_prompt,
        target,
        model,
        permission_mode: PermissionMode::Plan,
        resume_session_id: None,
        phase,
        step: "plan",
    })
    .await
    .with_context(|| format!("run claude-codes plan step for {phase}"))?;

    info!(phase, session_id = %session_id, "plan step complete");

    ensure_build_outputs(
        &runtime.claude_bin,
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
    claude_bin: &str,
    phase: &str,
    build_prompt: &str,
    model: &str,
    target: &Path,
    session_id: &str,
    expected_outputs: &[&Path],
) -> Result<()> {
    let mut prompt = build_prompt.to_string();

    for attempt in 1..=MAX_BUILD_ATTEMPTS {
        info!(phase, session_id = %session_id, attempt, "starting build step");
        run_step(StepRequest {
            claude_bin,
            prompt: &prompt,
            target,
            model,
            permission_mode: PermissionMode::AcceptEdits,
            resume_session_id: Some(session_id),
            phase,
            step: "build",
        })
        .await
        .with_context(|| format!("run claude-codes build step for {phase}"))?;

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

async fn run_step(req: StepRequest<'_>) -> Result<(String, Vec<ClaudeOutput>)> {
    let StepRequest {
        claude_bin,
        prompt,
        target,
        model,
        permission_mode,
        resume_session_id,
        phase,
        step,
    } = req;

    let mut builder = ClaudeCliBuilder::new()
        .command(claude_bin)
        .model(model)
        .permission_mode(permission_mode)
        .disallowed_tools(
            READ_ONLY_TOOLS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect::<Vec<_>>(),
        )
        .add_directories([target]);

    if let Some(session_id) = resume_session_id {
        builder = builder.resume(Some(session_id.to_string()));
    }

    let mut cmd = builder
        .build_command()
        .with_context(|| format!("build claude-codes {step} command for {phase}"))?;
    cmd.current_dir(target);

    let child = cmd
        .spawn()
        .with_context(|| format!("spawn claude-codes {step} process for {phase}"))?;

    let mut client = AsyncClient::new(child)
        .with_context(|| format!("create claude-codes {step} client for {phase}"))?;

    let responses = client
        .query(prompt)
        .await
        .with_context(|| format!("run claude-codes {step} query for {phase}"))?;

    if let Some(err) = responses.iter().find_map(|o| o.as_anthropic_error()) {
        bail!(
            "claude-codes {step} step failed for {phase}: {}",
            err.error.message
        );
    }

    let result = responses
        .iter()
        .rev()
        .find_map(ClaudeOutput::as_result)
        .with_context(|| format!("claude-codes {step} step did not return a result for {phase}"))?;

    if result.is_error {
        bail!(
            "claude-codes {step} step failed for {phase}: {}",
            result_error_message(result)
        );
    }

    let session_id = responses
        .iter()
        .rev()
        .find_map(|o| o.session_id().map(str::to_string))
        .or_else(|| Some(result.session_id.clone()))
        .with_context(|| {
            format!("claude-codes {step} step did not return a session id for {phase}")
        })?;

    Ok((session_id, responses))
}

fn result_error_message(result: &ResultMessage) -> String {
    if !result.errors.is_empty() {
        return result.errors.join("; ");
    }
    result
        .result
        .clone()
        .unwrap_or_else(|| "unknown error".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_accepts_known_values() {
        assert_eq!(parse_model("opus"), "opus");
        assert_eq!(parse_model("Haiku"), "haiku");
        assert_eq!(parse_model("sonnet"), "sonnet");
        assert_eq!(parse_model("kimi-k2.5"), "sonnet");
    }
}
