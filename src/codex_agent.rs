use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use codex_codes::{
    AbsolutePathBuf, AppServerBuilder, AskForApproval, AsyncClient,
    CommandExecutionApprovalDecision, CommandExecutionRequestApprovalResponse,
    FileChangeApprovalDecision, FileChangeRequestApprovalResponse, Notification, SandboxPolicy,
    ServerMessage, ServerRequest, ThreadStartParams, TurnStartParams, TurnStatus, UserInput,
};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::output::{
    log_missing_outputs, log_outputs_ready, missing_outputs, nudge_prompt, settle_outputs,
    verify_outputs,
};

const MAX_BUILD_ATTEMPTS: u32 = 3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnStep {
    Plan,
    Build,
}

struct TurnRequest<'a> {
    client: &'a mut AsyncClient,
    thread_id: &'a str,
    prompt: &'a str,
    model: Option<&'a str>,
    cwd: &'a str,
    step: TurnStep,
    sandbox_policy: SandboxPolicy,
    phase: &'a str,
    label: &'a str,
}

pub struct CodexRuntime {
    client: Arc<Mutex<AsyncClient>>,
}

pub async fn start_runtime(codex_bin: &str, target: &Path) -> Result<CodexRuntime> {
    let builder = AppServerBuilder::new()
        .command(codex_bin)
        .working_directory(target);
    let client = AsyncClient::start_with(builder)
        .await
        .context("start codex app-server")?;
    Ok(CodexRuntime {
        client: Arc::new(Mutex::new(client)),
    })
}

pub async fn stop_runtime(runtime: CodexRuntime) -> Result<()> {
    let client = Arc::try_unwrap(runtime.client)
        .map_err(|_| anyhow::anyhow!("codex app-server still in use"))?
        .into_inner();
    client
        .shutdown()
        .await
        .context("shutdown codex app-server")?;
    Ok(())
}

pub fn parse_model(model: &str) -> Option<String> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    if model == "kimi-k2.5" || model.starts_with("kimi-") {
        warn!(
            model,
            "model is the OpenCode default; using Codex CLI default model instead"
        );
        return None;
    }
    Some(model.to_string())
}

pub fn model_label(model: Option<&str>) -> &str {
    model.unwrap_or("default")
}

pub async fn run_plan_build_phase(
    runtime: &CodexRuntime,
    phase: &str,
    plan_prompt: &str,
    build_prompt: &str,
    model: Option<&str>,
    target: &Path,
    expected_outputs: &[&Path],
) -> Result<String> {
    let middleton_dir = target.join(".middleton");
    let cwd = target.to_string_lossy().into_owned();

    let mut client = runtime.client.lock().await;
    let thread = client
        .thread_start(&empty_thread_params()?)
        .await
        .with_context(|| format!("start codex thread for {phase}"))?;
    let thread_id = thread.thread.id.clone();

    info!(phase, agent = "codex-codes", thread_id = %thread_id, "starting plan turn");
    run_turn(TurnRequest {
        client: &mut client,
        thread_id: &thread_id,
        prompt: plan_prompt,
        model,
        cwd: &cwd,
        step: TurnStep::Plan,
        sandbox_policy: sandbox_for_plan(),
        phase,
        label: "plan",
    })
    .await?;

    info!(phase, thread_id = %thread_id, "plan turn complete");

    run_turn(TurnRequest {
        client: &mut client,
        thread_id: &thread_id,
        prompt: build_prompt,
        model,
        cwd: &cwd,
        step: TurnStep::Build,
        sandbox_policy: sandbox_for_build(&middleton_dir)?,
        phase,
        label: "build",
    })
    .await?;

    ensure_build_outputs(
        &runtime.client,
        &thread_id,
        model,
        &cwd,
        &middleton_dir,
        phase,
        expected_outputs,
    )
    .await?;

    info!(
        phase,
        thread_id = %thread_id,
        outputs = expected_outputs.len(),
        "phase complete"
    );

    Ok(thread_id)
}

async fn ensure_build_outputs(
    client: &Arc<Mutex<AsyncClient>>,
    thread_id: &str,
    model: Option<&str>,
    cwd: &str,
    middleton_dir: &Path,
    phase: &str,
    expected_outputs: &[&Path],
) -> Result<()> {
    for attempt in 1..=MAX_BUILD_ATTEMPTS {
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

        let prompt = nudge_prompt(&missing);
        let mut guard = client.lock().await;
        run_turn(TurnRequest {
            client: &mut guard,
            thread_id,
            prompt: &prompt,
            model,
            cwd,
            step: TurnStep::Build,
            sandbox_policy: sandbox_for_build(middleton_dir)?,
            phase,
            label: "build",
        })
        .await
        .with_context(|| format!("run codex-codes build nudge for {phase}"))?;
    }

    verify_outputs(expected_outputs, phase)
}

async fn run_turn(req: TurnRequest<'_>) -> Result<()> {
    let TurnRequest {
        client,
        thread_id,
        prompt,
        model,
        cwd,
        step,
        sandbox_policy,
        phase,
        label,
    } = req;

    client
        .turn_start(&TurnStartParams {
            thread_id: thread_id.to_string(),
            input: vec![UserInput::Text {
                text: prompt.to_string(),
                text_elements: None,
            }],
            approval_policy: Some(AskForApproval::OnRequest),
            approvals_reviewer: None,
            cwd: Some(cwd.to_string()),
            effort: None,
            model: model.map(str::to_string),
            output_schema: None,
            personality: None,
            sandbox_policy: Some(sandbox_policy),
            service_tier: None,
            summary: None,
        })
        .await
        .with_context(|| format!("start codex-codes {label} turn for {phase}"))?;

    drain_turn(client, step, phase, label).await
}

async fn drain_turn(
    client: &mut AsyncClient,
    step: TurnStep,
    phase: &str,
    label: &str,
) -> Result<()> {
    loop {
        let message = client
            .next_message()
            .await
            .with_context(|| format!("read codex-codes events for {phase} {label}"))?;
        let Some(message) = message else {
            bail!("codex app-server closed during {label} for {phase}");
        };

        match message {
            ServerMessage::Notification(notification) => {
                if let Some(error) = turn_error(&notification) {
                    bail!("codex-codes {label} failed for {phase}: {error}");
                }
                if matches!(notification, Notification::TurnCompleted(_)) {
                    return Ok(());
                }
            }
            ServerMessage::Request { id, request } => {
                respond_to_request(client, id, request, step).await?;
            }
        }
    }
}

fn turn_error(notification: &Notification) -> Option<String> {
    match notification {
        Notification::Error(event) => Some(event.error.message.clone()),
        Notification::TurnCompleted(event) => {
            if let Some(error) = &event.turn.error {
                return Some(error.message.clone());
            }
            if event.turn.status == TurnStatus::Failed {
                return Some("turn failed".to_string());
            }
            None
        }
        _ => None,
    }
}

async fn respond_to_request(
    client: &mut AsyncClient,
    id: codex_codes::RequestId,
    request: ServerRequest,
    step: TurnStep,
) -> Result<()> {
    match request {
        ServerRequest::CmdExecApproval(_) => {
            client
                .respond(
                    id,
                    &CommandExecutionRequestApprovalResponse {
                        decision: CommandExecutionApprovalDecision::Decline,
                    },
                )
                .await
                .context("decline command execution approval")?;
        }
        ServerRequest::FileChangeApproval(params) => {
            let decision = file_change_decision(&params, step);
            client
                .respond(id, &FileChangeRequestApprovalResponse { decision })
                .await
                .context("respond to file change approval")?;
        }
        ServerRequest::Unknown { method, .. } => {
            warn!(method, ?step, "ignoring unhandled codex server request");
        }
        other => {
            warn!(
                method = other.method(),
                ?step,
                "ignoring codex server request"
            );
        }
    }
    Ok(())
}

fn file_change_decision(
    params: &codex_codes::FileChangeRequestApprovalParams,
    step: TurnStep,
) -> FileChangeApprovalDecision {
    match step {
        TurnStep::Plan => FileChangeApprovalDecision::Decline,
        TurnStep::Build => {
            if grant_root_is_middleton(params.grant_root.as_deref()) {
                FileChangeApprovalDecision::Accept
            } else {
                FileChangeApprovalDecision::Decline
            }
        }
    }
}

fn grant_root_is_middleton(grant_root: Option<&str>) -> bool {
    grant_root.is_some_and(|root| root.contains(".middleton"))
}

fn empty_thread_params() -> Result<ThreadStartParams> {
    serde_json::from_value(serde_json::json!({})).context("build codex thread/start params")
}

fn sandbox_for_plan() -> SandboxPolicy {
    SandboxPolicy::ReadOnly {
        network_access: Some(false),
    }
}

fn sandbox_for_build(middleton_dir: &Path) -> Result<SandboxPolicy> {
    let writable = middleton_dir
        .canonicalize()
        .unwrap_or_else(|_| middleton_dir.to_path_buf());
    Ok(SandboxPolicy::WorkspaceWrite {
        exclude_slash_tmp: None,
        exclude_tmpdir_env_var: None,
        network_access: Some(false),
        writable_roots: Some(vec![AbsolutePathBuf(
            writable.to_string_lossy().into_owned(),
        )]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_passes_through_codex_ids() {
        assert_eq!(parse_model("gpt-5").as_deref(), Some("gpt-5"));
        assert_eq!(parse_model("  o4  ").as_deref(), Some("o4"));
    }

    #[test]
    fn parse_model_rejects_opencode_style_ids() {
        assert_eq!(parse_model("kimi-k2.5"), None);
    }
}
