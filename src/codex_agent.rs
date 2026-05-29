use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use codex_codes::ReviewDecision;

use anyhow::{Context, Result, bail};
use codex_codes::{
    AbsolutePathBuf, AppServerBuilder, AskForApproval, AsyncClient,
    CommandExecutionApprovalDecision, CommandExecutionRequestApprovalResponse,
    DynamicToolCallResponse, ExecCommandApprovalResponse, FileChangeApprovalDecision,
    FileChangeRequestApprovalResponse, GrantedPermissionProfile, McpServerElicitationAction,
    McpServerElicitationRequestResponse, Notification, PermissionsRequestApprovalResponse,
    SandboxPolicy, ServerMessage, ServerRequest, ThreadStartParams, ToolRequestUserInputAnswer,
    ToolRequestUserInputResponse, TurnStartParams, TurnStatus, UserInput,
};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::agent::ReviewProfile;
use crate::output::{
    log_missing_outputs, log_outputs_ready, missing_outputs, nudge_prompt, settle_outputs,
    verify_outputs,
};
use crate::run_log::{ConfirmedAction, RunLog};

const MAX_BUILD_ATTEMPTS: u32 = 3;
const DEFAULT_CODEX_INPUT_ANSWER: &str = "Use your best judgment and proceed without blocking.";

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
    profile: ReviewProfile,
    step: TurnStep,
    sandbox_policy: SandboxPolicy,
    phase: &'a str,
    label: &'a str,
    run_log: Arc<RunLog>,
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
    client.shutdown().await.context("stop codex app-server")
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

#[allow(clippy::too_many_arguments)]
pub async fn run_plan_build_phase(
    runtime: &CodexRuntime,
    phase: &str,
    plan_prompt: &str,
    build_prompt: &str,
    model: Option<&str>,
    profile: ReviewProfile,
    target: &Path,
    expected_outputs: &[&Path],
    run_log: Arc<RunLog>,
) -> Result<String> {
    let middleton_dir = expected_outputs
        .first()
        .and_then(|path| path.parent())
        .context("build step requires at least one expected output path")?;
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
        profile,
        step: TurnStep::Plan,
        sandbox_policy: sandbox_for_plan(),
        phase,
        label: "plan",
        run_log: Arc::clone(&run_log),
    })
    .await?;

    info!(phase, thread_id = %thread_id, "plan turn complete");

    run_turn(TurnRequest {
        client: &mut client,
        thread_id: &thread_id,
        prompt: build_prompt,
        model,
        cwd: &cwd,
        profile,
        step: TurnStep::Build,
        sandbox_policy: sandbox_for_build(middleton_dir)?,
        phase,
        label: "build",
        run_log: Arc::clone(&run_log),
    })
    .await?;

    ensure_build_outputs(
        &runtime.client,
        &thread_id,
        model,
        profile,
        &cwd,
        middleton_dir,
        phase,
        expected_outputs,
        run_log,
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

#[allow(clippy::too_many_arguments)]
async fn ensure_build_outputs(
    client: &Arc<Mutex<AsyncClient>>,
    thread_id: &str,
    model: Option<&str>,
    profile: ReviewProfile,
    cwd: &str,
    middleton_dir: &Path,
    phase: &str,
    expected_outputs: &[&Path],
    run_log: Arc<RunLog>,
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
            profile,
            step: TurnStep::Build,
            sandbox_policy: sandbox_for_build(middleton_dir)?,
            phase,
            label: "build",
            run_log: Arc::clone(&run_log),
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
        profile,
        step,
        sandbox_policy,
        phase,
        label,
        run_log,
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

    drain_turn(client, profile, step, phase, label, &run_log).await
}

async fn drain_turn(
    client: &mut AsyncClient,
    profile: ReviewProfile,
    step: TurnStep,
    phase: &str,
    label: &str,
    run_log: &RunLog,
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
                respond_to_request(client, id, request, profile, step, phase, run_log).await?;
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
    profile: ReviewProfile,
    step: TurnStep,
    phase: &str,
    run_log: &RunLog,
) -> Result<()> {
    let method = request.method().to_string();
    match request {
        ServerRequest::CmdExecApproval(_) => {
            let decision = command_execution_decision(profile, step);
            if decision == CommandExecutionApprovalDecision::Accept {
                log_codex_confirmed(
                    run_log,
                    phase,
                    step,
                    &method,
                    "command execution approved".to_string(),
                )?;
            }
            client
                .respond(id, &CommandExecutionRequestApprovalResponse { decision })
                .await
                .context("respond to command execution approval")?;
        }
        ServerRequest::ExecCommandApproval(params) => {
            let decision = exec_command_decision(profile, step);
            if decision == ReviewDecision::Approved {
                let command = params.command.join(" ");
                log_codex_confirmed(run_log, phase, step, &method, format!("command={command}"))?;
            }
            client
                .respond(id, &ExecCommandApprovalResponse { decision })
                .await
                .context("respond to exec command approval")?;
        }
        ServerRequest::FileChangeApproval(params) => {
            let decision = file_change_decision(&params, step);
            if decision == FileChangeApprovalDecision::Accept {
                let root = params.grant_root.as_deref().unwrap_or("unknown");
                log_codex_confirmed(run_log, phase, step, &method, format!("grant_root={root}"))?;
            }
            client
                .respond(id, &FileChangeRequestApprovalResponse { decision })
                .await
                .context("respond to file change approval")?;
        }
        ServerRequest::ApplyPatchApproval(params) => {
            let decision = apply_patch_decision(&params, step);
            if decision == ReviewDecision::Approved {
                let root = params.grant_root.as_deref().unwrap_or("unknown");
                log_codex_confirmed(
                    run_log,
                    phase,
                    step,
                    &method,
                    format!("apply_patch grant_root={root}"),
                )?;
            }
            client
                .respond(id, &codex_codes::ApplyPatchApprovalResponse { decision })
                .await
                .context("respond to apply patch approval")?;
        }
        ServerRequest::PermissionsRequestApproval(params) => {
            let response = permissions_approval_response(profile, step, &params.permissions);
            log_codex_confirmed(
                run_log,
                phase,
                step,
                &method,
                format!(
                    "permissions reason={}",
                    params.reason.as_deref().unwrap_or("")
                ),
            )?;
            client
                .respond(id, &response)
                .await
                .context("respond to permissions request approval")?;
        }
        ServerRequest::ToolRequestUserInput(params) => {
            let response = auto_tool_user_input_response(&params);
            let topics = params
                .questions
                .iter()
                .map(|q| q.header.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            log_codex_confirmed(
                run_log,
                phase,
                step,
                &method,
                format!("user_input topics=[{topics}]"),
            )?;
            client
                .respond(id, &response)
                .await
                .context("respond to tool user input request")?;
        }
        ServerRequest::McpServerElicitationRequest(_) => {
            let action = mcp_elicitation_action(profile, step);
            if action == McpServerElicitationAction::Accept {
                log_codex_confirmed(
                    run_log,
                    phase,
                    step,
                    &method,
                    "mcp elicitation accepted".to_string(),
                )?;
            }
            let response = McpServerElicitationRequestResponse {
                _meta: None,
                action,
                content: None,
            };
            client
                .respond(id, &response)
                .await
                .context("respond to mcp elicitation request")?;
        }
        ServerRequest::ItemToolCall(params) => {
            let success = plan_dynamic_tool_allowed(profile, step, &params.tool);
            if success {
                log_codex_confirmed(
                    run_log,
                    phase,
                    step,
                    &method,
                    format!("tool={} call_id={}", params.tool, params.call_id),
                )?;
            }
            let response = DynamicToolCallResponse {
                content_items: Vec::new(),
                success,
            };
            client
                .respond(id, &response)
                .await
                .context("respond to dynamic tool call")?;
        }
        ServerRequest::ChatgptAuthTokensRefresh(_) | ServerRequest::AttestationGenerate(_) => {
            client
                .respond(id, &serde_json::json!({ "approved": false }))
                .await
                .context("decline codex auth/attestation request")?;
        }
        ServerRequest::Unknown { params, .. } => {
            let payload = unknown_request_response(profile, step, params.as_ref());
            if payload
                .get("decision")
                .and_then(|value| value.as_str())
                .is_some_and(|decision| decision == "accept")
            {
                log_codex_confirmed(
                    run_log,
                    phase,
                    step,
                    &method,
                    "unknown request accepted".to_string(),
                )?;
            }
            client
                .respond(id, &payload)
                .await
                .with_context(|| format!("respond to unknown codex request {method}"))?;
        }
    }

    Ok(())
}

fn log_codex_confirmed(
    run_log: &RunLog,
    phase: &str,
    step: TurnStep,
    method: &str,
    detail: String,
) -> Result<()> {
    run_log.record_confirmed(ConfirmedAction {
        agent: "codex",
        phase,
        step: turn_step_label(step),
        kind: "approval",
        detail: format!("{method}: {detail}"),
    })
}

fn turn_step_label(step: TurnStep) -> &'static str {
    match step {
        TurnStep::Plan => "plan",
        TurnStep::Build => "build",
    }
}

fn command_execution_decision(
    profile: ReviewProfile,
    step: TurnStep,
) -> CommandExecutionApprovalDecision {
    match step {
        TurnStep::Plan if profile.plan_allows_command_execution() => {
            CommandExecutionApprovalDecision::Accept
        }
        TurnStep::Plan | TurnStep::Build => CommandExecutionApprovalDecision::Decline,
    }
}

fn exec_command_decision(profile: ReviewProfile, step: TurnStep) -> ReviewDecision {
    if step == TurnStep::Plan && profile.plan_allows_command_execution() {
        ReviewDecision::Approved
    } else {
        ReviewDecision::Denied
    }
}

fn apply_patch_decision(
    params: &codex_codes::ApplyPatchApprovalParams,
    step: TurnStep,
) -> ReviewDecision {
    match step {
        TurnStep::Plan => ReviewDecision::Denied,
        TurnStep::Build if grant_root_is_middleton(params.grant_root.as_deref()) => {
            ReviewDecision::Approved
        }
        TurnStep::Build => ReviewDecision::Denied,
    }
}

fn permissions_approval_response(
    profile: ReviewProfile,
    step: TurnStep,
    requested: &codex_codes::RequestPermissionProfile,
) -> PermissionsRequestApprovalResponse {
    let permissions = match step {
        TurnStep::Plan => GrantedPermissionProfile {
            file_system: requested.file_system.clone(),
            network: if profile.plan_allows_web_research("network") {
                requested.network.clone()
            } else {
                None
            },
        },
        TurnStep::Build => GrantedPermissionProfile {
            file_system: requested.file_system.clone(),
            network: None,
        },
    };

    PermissionsRequestApprovalResponse {
        permissions,
        scope: None,
        strict_auto_review: None,
    }
}

fn auto_tool_user_input_response(
    params: &codex_codes::ToolRequestUserInputParams,
) -> ToolRequestUserInputResponse {
    let mut answers = BTreeMap::new();
    for question in &params.questions {
        let answer = question
            .options
            .as_ref()
            .and_then(|options| options.first())
            .map(|option| option.label.clone())
            .unwrap_or_else(|| DEFAULT_CODEX_INPUT_ANSWER.to_string());
        answers.insert(
            question.id.clone(),
            ToolRequestUserInputAnswer {
                answers: vec![answer],
            },
        );
    }
    ToolRequestUserInputResponse { answers }
}

fn mcp_elicitation_action(profile: ReviewProfile, step: TurnStep) -> McpServerElicitationAction {
    if step == TurnStep::Plan && profile.plan_allows_web_research("network") {
        McpServerElicitationAction::Accept
    } else {
        McpServerElicitationAction::Decline
    }
}

fn plan_dynamic_tool_allowed(profile: ReviewProfile, step: TurnStep, tool: &str) -> bool {
    if step != TurnStep::Plan {
        return false;
    }
    let tool = tool.to_ascii_lowercase();
    if profile.plan_allows_web_research("search")
        && (tool.contains("web") || tool.contains("search") || tool.contains("fetch"))
    {
        return true;
    }
    profile.plan_allows_command_execution()
        && (tool.contains("shell") || tool.contains("bash") || tool.contains("command"))
}

fn unknown_request_response(
    profile: ReviewProfile,
    step: TurnStep,
    params: Option<&serde_json::Value>,
) -> serde_json::Value {
    if step == TurnStep::Plan
        && (profile.plan_allows_web_research("network") || profile.plan_allows_command_execution())
    {
        if let Some(params) = params
            && params.get("questions").is_some()
        {
            return serde_json::json!({
                "answers": {}
            });
        }
        return serde_json::json!({ "decision": "accept" });
    }
    serde_json::json!({ "decision": "decline" })
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
        network_access: Some(true),
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

    #[test]
    fn plan_web_tool_allowed_for_documents() {
        assert!(plan_dynamic_tool_allowed(
            ReviewProfile::Documents,
            TurnStep::Plan,
            "web_search"
        ));
        assert!(!plan_dynamic_tool_allowed(
            ReviewProfile::Documents,
            TurnStep::Plan,
            "shell"
        ));
    }

    #[test]
    fn plan_shell_tool_allowed_for_repository() {
        assert!(plan_dynamic_tool_allowed(
            ReviewProfile::Repository,
            TurnStep::Plan,
            "local_shell"
        ));
    }
}
