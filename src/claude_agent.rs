use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use claude_codes::{
    AsyncClient, ClaudeCliBuilder, ClaudeInput, ClaudeOutput, ContentBlock, ControlRequestPayload,
    ControlResponse, PermissionMode, ToolPermissionRequest, ToolResultBlock, ToolResultContent,
    io::ResultMessage,
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::agent::ReviewProfile;
use crate::output::{
    log_missing_outputs, log_outputs_ready, missing_outputs, nudge_prompt, settle_outputs,
    verify_outputs,
};
use crate::run_log::{ConfirmedAction, RunLog};

const SHELL_TOOLS: &[&str] = &["Bash"];
const WEB_TOOLS: &[&str] = &["WebFetch", "WebSearch"];
const BUILD_DISALLOWED_TOOLS: &[&str] = &["Bash", "WebFetch", "WebSearch"];
const MAX_BUILD_ATTEMPTS: u32 = 3;
const DEFAULT_QUESTION_ANSWER: &str = "Use your best judgment and proceed without blocking.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Plan,
    Build,
}

struct StepRequest<'a> {
    claude_bin: &'a str,
    prompt: &'a str,
    target: &'a Path,
    model: &'a str,
    profile: ReviewProfile,
    permission_mode: PermissionMode,
    resume_session_id: Option<&'a str>,
    phase: &'a str,
    step: StepKind,
    run_log: Arc<RunLog>,
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
                "using Claude sonnet model; claude accepts sonnet, opus, or haiku"
            );
            "sonnet"
        }
    }
}

pub fn model_label(model: &str) -> &str {
    model
}

#[allow(clippy::too_many_arguments)]
pub async fn run_plan_build_phase(
    runtime: &ClaudeCodeRuntime,
    phase: &str,
    plan_prompt: &str,
    build_prompt: &str,
    model: &str,
    profile: ReviewProfile,
    target: &Path,
    expected_outputs: &[&Path],
    run_log: Arc<RunLog>,
) -> Result<String> {
    info!(phase, agent = "claude", "starting plan step");
    let (session_id, _) = run_step(StepRequest {
        claude_bin: &runtime.claude_bin,
        prompt: plan_prompt,
        target,
        model,
        profile,
        permission_mode: PermissionMode::Plan,
        resume_session_id: None,
        phase,
        step: StepKind::Plan,
        run_log: Arc::clone(&run_log),
    })
    .await
    .with_context(|| format!("run claude plan step for {phase}"))?;

    info!(phase, session_id = %session_id, "plan step complete");

    ensure_build_outputs(
        &runtime.claude_bin,
        phase,
        build_prompt,
        model,
        profile,
        target,
        &session_id,
        expected_outputs,
        run_log,
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

#[allow(clippy::too_many_arguments)]
async fn ensure_build_outputs(
    claude_bin: &str,
    phase: &str,
    build_prompt: &str,
    model: &str,
    profile: ReviewProfile,
    target: &Path,
    session_id: &str,
    expected_outputs: &[&Path],
    run_log: Arc<RunLog>,
) -> Result<()> {
    let mut prompt = build_prompt.to_string();

    for attempt in 1..=MAX_BUILD_ATTEMPTS {
        info!(phase, session_id = %session_id, attempt, "starting build step");
        run_step(StepRequest {
            claude_bin,
            prompt: &prompt,
            target,
            model,
            profile,
            permission_mode: PermissionMode::AcceptEdits,
            resume_session_id: Some(session_id),
            phase,
            step: StepKind::Build,
            run_log: Arc::clone(&run_log),
        })
        .await
        .with_context(|| format!("run claude build step for {phase}"))?;

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
        profile,
        permission_mode,
        resume_session_id,
        phase,
        step,
        run_log,
    } = req;

    let mut builder = ClaudeCliBuilder::new()
        .command(claude_bin)
        .model(model)
        .permission_mode(permission_mode)
        .permission_prompt_tool("stdio")
        .add_directories([target]);

    builder = builder.disallowed_tools(disallowed_tools_for_step(profile, step));

    if let Some(session_id) = resume_session_id {
        builder = builder.resume(Some(session_id.to_string()));
    }

    let mut cmd = builder
        .build_command()
        .with_context(|| format!("build claude {step:?} command for {phase}"))?;
    cmd.current_dir(target);

    let child = cmd
        .spawn()
        .with_context(|| format!("spawn claude {step:?} process for {phase}"))?;

    let mut client = AsyncClient::new(child)
        .with_context(|| format!("create claude {step:?} client for {phase}"))?;

    let responses =
        query_with_control_handling(&mut client, prompt, profile, step, phase, target, &run_log)
            .await
            .with_context(|| format!("run claude {step:?} query for {phase}"))?;

    if let Some(err) = responses.iter().find_map(|o| o.as_anthropic_error()) {
        bail!(
            "claude {step:?} step failed for {phase}: {}",
            err.error.message
        );
    }

    let result = responses
        .iter()
        .rev()
        .find_map(ClaudeOutput::as_result)
        .with_context(|| {
            format!("claude {step:?} step did not return a result for {phase}")
        })?;

    if result.is_error {
        bail!(
            "claude {step:?} step failed for {phase}: {}",
            result_error_message(result)
        );
    }

    let session_id = responses
        .iter()
        .rev()
        .find_map(|o| o.session_id().map(str::to_string))
        .or_else(|| Some(result.session_id.clone()))
        .with_context(|| {
            format!("claude {step:?} step did not return a session id for {phase}")
        })?;

    Ok((session_id, responses))
}

/// Drive the Claude session until a result arrives, auto-answering permission prompts.
fn disallowed_tools_for_step(profile: ReviewProfile, step: StepKind) -> Vec<String> {
    match step {
        StepKind::Build => BUILD_DISALLOWED_TOOLS
            .iter()
            .map(|tool| (*tool).to_string())
            .collect(),
        StepKind::Plan if profile == ReviewProfile::Documents => {
            SHELL_TOOLS.iter().map(|tool| (*tool).to_string()).collect()
        }
        StepKind::Plan => Vec::new(),
    }
}

async fn query_with_control_handling(
    client: &mut AsyncClient,
    prompt: &str,
    profile: ReviewProfile,
    step: StepKind,
    phase: &str,
    target: &Path,
    run_log: &RunLog,
) -> Result<Vec<ClaudeOutput>> {
    client
        .enable_tool_approval()
        .await
        .context("enable claude tool approval protocol")?;

    let session_id = Uuid::new_v4();
    client
        .send(&ClaudeInput::user_message(prompt, session_id))
        .await
        .context("send claude user message")?;

    let mut responses = Vec::new();
    let mut active_session = session_id;
    loop {
        let output = client
            .receive()
            .await
            .context("receive claude message")?;

        if let Some(session) = output_session_id(&output) {
            active_session = session;
        }

        if let ClaudeOutput::ControlRequest(req) = &output {
            match &req.request {
                ControlRequestPayload::CanUseTool(perm) => {
                    let response = permission_response(
                        perm,
                        &req.request_id,
                        profile,
                        step,
                        phase,
                        target,
                        run_log,
                    )?;
                    client
                        .send_control_response(response)
                        .await
                        .context("send claude control response")?;
                }
                ControlRequestPayload::HookCallback(_) | ControlRequestPayload::McpMessage(_) => {
                    client
                        .send_control_response(ControlResponse::success_empty(&req.request_id))
                        .await
                        .context("ack claude hook/mcp control request")?;
                }
                ControlRequestPayload::Initialize(_) => {
                    client
                        .send_control_response(ControlResponse::success_empty(&req.request_id))
                        .await
                        .context("ack claude initialize control request")?;
                }
            }
            continue;
        }

        if let ClaudeOutput::User(user) = &output
            && let Some(reply) = interactive_follow_up(&user.message.content, step)
        {
            run_log.record_confirmed(ConfirmedAction {
                agent: "claude",
                phase,
                step: step_kind_label(step),
                kind: "interactive",
                detail: truncate(&reply, 240),
            })?;
            client
                .send(&ClaudeInput::user_message(
                    reply,
                    user.session_id.unwrap_or(active_session),
                ))
                .await
                .context("send claude interactive follow-up")?;
        }

        let finished = matches!(&output, ClaudeOutput::Result(_));
        responses.push(output);
        if finished {
            break;
        }
    }

    Ok(responses)
}

fn output_session_id(output: &ClaudeOutput) -> Option<Uuid> {
    match output {
        ClaudeOutput::User(user) => user.session_id,
        ClaudeOutput::Assistant(assistant) => Uuid::parse_str(&assistant.session_id).ok(),
        ClaudeOutput::Result(result) => Uuid::parse_str(&result.session_id).ok(),
        ClaudeOutput::System(system) => system
            .data
            .get("session_id")
            .and_then(|value| value.as_str())
            .and_then(|id| Uuid::parse_str(id).ok()),
        _ => output.session_id().and_then(|id| Uuid::parse_str(id).ok()),
    }
}

/// When enterprise policy blocks tools, the CLI may echo interactive prompts as user
/// `tool_result` messages (e.g. "Exit plan mode?") instead of stdio control requests.
fn interactive_follow_up(content: &[ContentBlock], step: StepKind) -> Option<String> {
    for block in content {
        let ContentBlock::ToolResult(result) = block else {
            continue;
        };
        let prompt = tool_result_text(result)?;
        if result.is_error == Some(true) || looks_interactive_prompt(&prompt) {
            return Some(auto_reply_for_prompt(&prompt, step));
        }
    }
    None
}

fn tool_result_text(result: &ToolResultBlock) -> Option<String> {
    match &result.content {
        Some(ToolResultContent::Text(text)) => Some(text.clone()),
        Some(ToolResultContent::Structured(values)) => {
            let joined = values
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        None => None,
    }
}

fn looks_interactive_prompt(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("exit plan mode")
        || lower.contains('?')
        || lower.contains("company policy")
        || lower.contains("permission denied")
        || lower.contains("blocked by policy")
        || lower.contains("not permitted")
        || lower.contains("requires approval")
        || lower.contains("askuserquestion")
        || lower.contains("answer your questions")
        || lower.contains("allow ")
        || lower.contains("approve")
        || lower.contains("confirm")
        || lower.contains("websearch")
        || lower.contains("web fetch")
        || lower.contains("webfetch")
        || lower.contains("proceed with")
        || lower.contains("would you like")
}

fn auto_reply_for_prompt(prompt: &str, step: StepKind) -> String {
    let lower = prompt.to_ascii_lowercase();
    if lower.contains("exit plan mode") {
        return "Yes. Exit plan mode and continue with the middleton workflow. \
Do not request further interactive confirmation."
            .to_string();
    }
    if lower.contains("answer your questions") || lower.contains("askuserquestion") {
        return format!("{DEFAULT_QUESTION_ANSWER} Proceed without blocking.");
    }
    if lower.contains("websearch")
        || lower.contains("web fetch")
        || lower.contains("webfetch")
        || (lower.contains("search") && lower.contains("allow"))
    {
        return "Yes. Proceed with read-only web search for corroboration. \
Label findings as external context."
            .to_string();
    }
    if lower.contains("allow") || lower.contains("approve") || lower.contains("confirm") {
        return "Yes. Approved — continue without waiting for a human.".to_string();
    }
    match step {
        StepKind::Plan => "Continue planning in read-only mode. Do not run commands, \
do not write files, and do not ask for interactive confirmation."
            .to_string(),
        StepKind::Build => "Continue the build step. Write only under .middleton/ and \
do not ask for interactive confirmation."
            .to_string(),
    }
}

fn step_kind_label(step: StepKind) -> &'static str {
    match step {
        StepKind::Plan => "plan",
        StepKind::Build => "build",
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

fn permission_response(
    perm: &ToolPermissionRequest,
    request_id: &str,
    profile: ReviewProfile,
    step: StepKind,
    phase: &str,
    target: &Path,
    run_log: &RunLog,
) -> Result<ControlResponse> {
    if perm.tool_name == "AskUserQuestion" {
        let answers = default_question_answers(perm)?;
        run_log.record_confirmed(ConfirmedAction {
            agent: "claude",
            phase,
            step: step_kind_label(step),
            kind: "question",
            detail: format!("tool=AskUserQuestion request_id={request_id}"),
        })?;
        return perm
            .answer_questions(&answers, request_id)
            .context("answer AskUserQuestion control request");
    }

    if is_execution_tool(&perm.tool_name, profile, step) {
        return Ok(perm.deny(
            "middleton does not allow command execution during review",
            request_id,
        ));
    }

    match step {
        StepKind::Plan => {
            if perm.tool_name == "ExitPlanMode"
                || is_read_tool(&perm.tool_name)
                || plan_tool_allowed(profile, &perm.tool_name)
            {
                log_claude_tool_confirmed(run_log, phase, step, perm)?;
                return Ok(perm.allow(request_id));
            }
            if is_write_tool(&perm.tool_name) {
                return Ok(perm.deny(
                    "middleton plan step is read-only; write during the build step",
                    request_id,
                ));
            }
            log_claude_tool_confirmed(run_log, phase, step, perm)?;
            Ok(perm.allow(request_id))
        }
        StepKind::Build => {
            if perm.tool_name == "ExitPlanMode" {
                log_claude_tool_confirmed(run_log, phase, step, perm)?;
                return Ok(perm.allow(request_id));
            }
            if is_read_tool(&perm.tool_name) {
                log_claude_tool_confirmed(run_log, phase, step, perm)?;
                return Ok(perm.allow(request_id));
            }
            if is_write_tool(&perm.tool_name) && writes_middleton_only(perm, target) {
                log_claude_tool_confirmed(run_log, phase, step, perm)?;
                return Ok(perm.allow(request_id));
            }
            if is_write_tool(&perm.tool_name) {
                return Ok(perm.deny(
                    "middleton only allows writes under .middleton/ during build",
                    request_id,
                ));
            }
            Ok(perm.deny("tool not permitted during middleton build step", request_id))
        }
    }
}

fn default_question_answers(perm: &ToolPermissionRequest) -> Result<HashMap<usize, String>> {
    use claude_codes::AskUserQuestionInput;

    let parsed: AskUserQuestionInput =
        serde_json::from_value(perm.input.clone()).context("parse AskUserQuestion input")?;

    let mut answers = HashMap::new();
    for (index, question) in parsed.questions.iter().enumerate() {
        let answer = question
            .options
            .first()
            .map(|option| option.label.clone())
            .unwrap_or_else(|| DEFAULT_QUESTION_ANSWER.to_string());
        answers.insert(index, answer);
    }
    Ok(answers)
}

fn log_claude_tool_confirmed(
    run_log: &RunLog,
    phase: &str,
    step: StepKind,
    perm: &ToolPermissionRequest,
) -> Result<()> {
    let input = serde_json::to_string(&perm.input).unwrap_or_else(|_| perm.input.to_string());
    run_log.record_confirmed(ConfirmedAction {
        agent: "claude",
        phase,
        step: step_kind_label(step),
        kind: "tool",
        detail: format!("tool={} input={}", perm.tool_name, truncate(&input, 200)),
    })
}

fn plan_tool_allowed(profile: ReviewProfile, tool: &str) -> bool {
    is_web_tool(tool) || (profile == ReviewProfile::Repository && is_shell_tool(tool))
}

fn is_execution_tool(tool: &str, profile: ReviewProfile, step: StepKind) -> bool {
    if step == StepKind::Plan && plan_tool_allowed(profile, tool) {
        return false;
    }
    let tool = tool.to_ascii_lowercase();
    tool.contains("bash")
        || tool.contains("command")
        || tool.contains("terminal")
        || tool.contains("execute")
        || tool.contains("run")
        || tool.contains("install")
        || tool.contains("network")
        || tool.contains("fetch")
        || is_web_tool(&tool)
        || is_shell_tool(&tool)
}

fn is_web_tool(tool: &str) -> bool {
    WEB_TOOLS
        .iter()
        .any(|allowed| tool.eq_ignore_ascii_case(allowed))
}

fn is_shell_tool(tool: &str) -> bool {
    SHELL_TOOLS
        .iter()
        .any(|allowed| tool.eq_ignore_ascii_case(allowed))
}

fn is_read_tool(tool: &str) -> bool {
    matches!(
        tool,
        "Read" | "Glob" | "Grep" | "Ls" | "List" | "Search" | "View"
    )
}

fn is_write_tool(tool: &str) -> bool {
    matches!(tool, "Write" | "Edit" | "NotebookEdit" | "MultiEdit")
}

fn writes_middleton_only(perm: &ToolPermissionRequest, target: &Path) -> bool {
    let middleton = target.join(".middleton");
    let middleton_prefix = middleton.to_string_lossy();

    tool_paths(perm)
        .iter()
        .any(|path| path.contains(".middleton/") || path.contains(&*middleton_prefix))
}

fn tool_paths(perm: &ToolPermissionRequest) -> Vec<String> {
    let mut paths = Vec::new();
    for key in ["file_path", "path", "notebook_path"] {
        if let Some(value) = perm.input.get(key).and_then(|v| v.as_str()) {
            paths.push(value.to_string());
        }
    }
    paths
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
    use std::sync::Arc;

    use super::*;
    use crate::agent::AgentKind;
    use crate::paths::ArtifactPaths;
    use serde_json::json;

    fn test_run_log() -> Arc<RunLog> {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("repo");
        std::fs::create_dir_all(&target).unwrap();
        Arc::new(RunLog::open(&ArtifactPaths::new(&target, AgentKind::OpenCode)).unwrap())
    }

    fn permission_decision_label(response: &ControlResponse) -> &'static str {
        if response_allows(response) {
            "allow"
        } else {
            "deny"
        }
    }

    fn response_allows(response: &ControlResponse) -> bool {
        let claude_codes::ControlResponsePayload::Success {
            response: Some(body),
            ..
        } = &response.response
        else {
            return false;
        };
        body.get("behavior")
            .and_then(|value| value.as_str())
            .is_some_and(|behavior| behavior == "allow")
    }

    #[test]
    fn parse_model_accepts_known_values() {
        assert_eq!(parse_model("opus"), "opus");
        assert_eq!(parse_model("Haiku"), "haiku");
        assert_eq!(parse_model("sonnet"), "sonnet");
        assert_eq!(parse_model("kimi-k2.5"), "sonnet");
    }

    #[test]
    fn plan_step_allows_exit_plan_mode() {
        let perm = ToolPermissionRequest {
            tool_name: "ExitPlanMode".to_string(),
            input: json!({}),
            permission_suggestions: vec![],
            blocked_path: None,
            decision_reason: None,
            tool_use_id: None,
        };
        let log = test_run_log();
        let response = permission_response(
            &perm,
            "req-1",
            ReviewProfile::Repository,
            StepKind::Plan,
            "test",
            Path::new("/repo"),
            &log,
        )
        .unwrap();
        assert_eq!(permission_decision_label(&response), "allow");
    }

    #[test]
    fn plan_step_denies_write() {
        let perm = ToolPermissionRequest {
            tool_name: "Write".to_string(),
            input: json!({"file_path": "/repo/.middleton/DEPTH.md"}),
            permission_suggestions: vec![],
            blocked_path: None,
            decision_reason: None,
            tool_use_id: None,
        };
        let log = test_run_log();
        let response = permission_response(
            &perm,
            "req-1",
            ReviewProfile::Repository,
            StepKind::Plan,
            "test",
            Path::new("/repo"),
            &log,
        )
        .unwrap();
        assert_eq!(permission_decision_label(&response), "deny");
    }

    #[test]
    fn detects_exit_plan_mode_tool_result_prompt() {
        let json = r#"{
            "type":"user",
            "message":{"role":"user","content":[{"type":"tool_result","content":"Exit plan mode?","is_error":true,"tool_use_id":"toolu_test"}]},
            "session_id":"fb073375-9d58-4ffe-912f-6bc5933256f8"
        }"#;
        let output: ClaudeOutput = serde_json::from_str(json).unwrap();
        let ClaudeOutput::User(user) = output else {
            panic!("expected user message");
        };
        let reply =
            interactive_follow_up(&user.message.content, StepKind::Plan).expect("follow-up");
        assert!(reply.to_ascii_lowercase().contains("exit plan mode"));
    }

    #[test]
    fn plan_step_allows_web_search() {
        let perm = ToolPermissionRequest {
            tool_name: "WebSearch".to_string(),
            input: json!({}),
            permission_suggestions: vec![],
            blocked_path: None,
            decision_reason: None,
            tool_use_id: None,
        };
        let log = test_run_log();
        let response = permission_response(
            &perm,
            "req-1",
            ReviewProfile::Repository,
            StepKind::Plan,
            "test",
            Path::new("/repo"),
            &log,
        )
        .unwrap();
        assert_eq!(permission_decision_label(&response), "allow");
    }

    #[test]
    fn documents_plan_denies_bash() {
        let perm = ToolPermissionRequest {
            tool_name: "Bash".to_string(),
            input: json!({}),
            permission_suggestions: vec![],
            blocked_path: None,
            decision_reason: None,
            tool_use_id: None,
        };
        let log = test_run_log();
        let response = permission_response(
            &perm,
            "req-1",
            ReviewProfile::Documents,
            StepKind::Plan,
            "test",
            Path::new("/repo"),
            &log,
        )
        .unwrap();
        assert_eq!(permission_decision_label(&response), "deny");
    }

    #[test]
    fn build_step_denies_web_search() {
        let perm = ToolPermissionRequest {
            tool_name: "WebSearch".to_string(),
            input: json!({}),
            permission_suggestions: vec![],
            blocked_path: None,
            decision_reason: None,
            tool_use_id: None,
        };
        let log = test_run_log();
        let response = permission_response(
            &perm,
            "req-1",
            ReviewProfile::Repository,
            StepKind::Build,
            "test",
            Path::new("/repo"),
            &log,
        )
        .unwrap();
        assert_eq!(permission_decision_label(&response), "deny");
    }

    #[test]
    fn build_step_allows_middleton_write() {
        let perm = ToolPermissionRequest {
            tool_name: "Write".to_string(),
            input: json!({"file_path": "/repo/.middleton/opencode/DEPTH.md"}),
            permission_suggestions: vec![],
            blocked_path: None,
            decision_reason: None,
            tool_use_id: None,
        };
        let log = test_run_log();
        let response = permission_response(
            &perm,
            "req-1",
            ReviewProfile::Repository,
            StepKind::Build,
            "test",
            Path::new("/repo"),
            &log,
        )
        .unwrap();
        assert_eq!(permission_decision_label(&response), "allow");
    }
}
