use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use opencode_rs::Client;
use opencode_rs::types::event::Event;
use opencode_rs::types::message::{PromptPart, PromptRequest};
use opencode_rs::types::permission::{PermissionReply, PermissionReplyRequest};
use opencode_rs::types::project::ModelRef;
use opencode_rs::types::question::QuestionReply;
use opencode_rs::types::session::{CreateSessionRequest, SessionStatusInfo};
use tracing::{debug, error, info, warn};

const IDLE_GRACE: Duration = Duration::from_secs(2);
const SESSION_DEADLINE: Duration = Duration::from_secs(3600);
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_QUESTION_ANSWER: &str = "Use your best judgment and proceed without blocking.";

pub async fn run_plan_build_phase(
    client: &Client,
    phase: &str,
    plan_prompt: &str,
    build_prompt: &str,
    model: &ModelRef,
    expected_outputs: &[&Path],
) -> Result<String> {
    let session = client
        .sessions()
        .create(&CreateSessionRequest {
            title: Some(format!("middleton: {phase}")),
            ..Default::default()
        })
        .await
        .with_context(|| format!("create session for {phase}"))?;

    info!(phase, session_id = %session.id, "session created");

    let mut subscription = client
        .subscribe_session(&session.id)
        .with_context(|| format!("subscribe to session events for {phase}"))?;

    send_prompt(client, &session.id, plan_prompt, model, "plan").await?;
    wait_until_idle(client, &session.id, &mut subscription, true).await?;

    send_prompt(client, &session.id, build_prompt, model, "build").await?;
    wait_until_idle(client, &session.id, &mut subscription, true).await?;

    for output in expected_outputs {
        verify_output(output, phase)?;
    }
    info!(
        phase,
        session_id = %session.id,
        outputs = expected_outputs.len(),
        "phase complete"
    );

    Ok(session.id)
}

async fn send_prompt(
    client: &Client,
    session_id: &str,
    text: &str,
    model: &ModelRef,
    agent: &str,
) -> Result<()> {
    let request = PromptRequest {
        parts: vec![PromptPart::Text {
            text: text.to_string(),
            synthetic: None,
            ignored: None,
            metadata: None,
        }],
        message_id: None,
        model: Some(model.clone()),
        agent: Some(agent.to_string()),
        no_reply: None,
        system: None,
        variant: None,
    };

    client
        .messages()
        .prompt_async(session_id, &request)
        .await
        .with_context(|| format!("send {agent} prompt to session {session_id}"))?;

    debug!(session_id, agent, "prompt dispatched");
    Ok(())
}

async fn wait_until_idle(
    client: &Client,
    session_id: &str,
    subscription: &mut opencode_rs::sse::SseSubscription<Event>,
    dispatched_new_work: bool,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + SESSION_DEADLINE;
    let mut last_activity = tokio::time::Instant::now();
    let idle_grace_deadline = Some(tokio::time::Instant::now() + IDLE_GRACE);
    let mut awaiting_idle_grace_check = false;
    let mut observed_busy = false;
    let mut sse_active = true;

    let mut poll_interval = tokio::time::interval(Duration::from_secs(1));
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let now = tokio::time::Instant::now();
        if now.duration_since(last_activity) >= INACTIVITY_TIMEOUT {
            bail!("session {session_id} timed out after 5 minutes without activity");
        }
        if now >= deadline {
            bail!("session {session_id} timed out after 1 hour");
        }

        tokio::select! {
            maybe_event = subscription.recv(), if sse_active => {
                let Some(event) = maybe_event else {
                    warn!(session_id, "SSE stream closed; falling back to polling");
                    sse_active = false;
                    continue;
                };

                if handle_event(client, session_id, event, &mut last_activity).await? {
                    return Ok(());
                }
                observed_busy = true;
                awaiting_idle_grace_check = false;
            }

            _ = poll_interval.tick() => {
                if handle_pending_interactions(client, session_id).await? {
                    observed_busy = true;
                    last_activity = tokio::time::Instant::now();
                    continue;
                }

                match client.sessions().status_for(session_id).await {
                    Ok(SessionStatusInfo::Busy | SessionStatusInfo::Retry { .. }) => {
                        last_activity = tokio::time::Instant::now();
                        observed_busy = true;
                        awaiting_idle_grace_check = false;
                    }
                    Ok(SessionStatusInfo::Idle) => {
                        if !dispatched_new_work || observed_busy {
                            debug!(session_id, "session idle via polling");
                            return Ok(());
                        }

                        let Some(grace_deadline) = idle_grace_deadline else {
                            continue;
                        };

                        if tokio::time::Instant::now() >= grace_deadline {
                            debug!(session_id, "session idle accepted after grace period");
                            return Ok(());
                        }

                        awaiting_idle_grace_check = true;
                    }
                    Err(error) => {
                        warn!(session_id, %error, "failed to poll session status");
                    }
                }
            }

            () = async {
                match idle_grace_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            }, if awaiting_idle_grace_check => {
                awaiting_idle_grace_check = false;
                match client.sessions().status_for(session_id).await {
                    Ok(SessionStatusInfo::Idle) => {
                        debug!(session_id, "session idle after grace wait");
                        return Ok(());
                    }
                    Ok(SessionStatusInfo::Busy | SessionStatusInfo::Retry { .. }) => {
                        last_activity = tokio::time::Instant::now();
                        observed_busy = true;
                    }
                    Err(error) => {
                        warn!(session_id, %error, "status check failed at idle grace deadline");
                    }
                }
            }
        }
    }
}

async fn handle_event(
    client: &Client,
    session_id: &str,
    event: Event,
    last_activity: &mut tokio::time::Instant,
) -> Result<bool> {
    match event {
        Event::SessionIdle { .. } => {
            debug!(session_id, "received SessionIdle");
            Ok(true)
        }
        Event::SessionError { properties } => {
            let message = properties
                .error
                .map(|error| format!("{error:?}"))
                .unwrap_or_else(|| "unknown session error".to_string());
            error!(session_id, error = %message, "session error");
            bail!("session {session_id} failed: {message}");
        }
        Event::PermissionAsked { properties } => {
            reply_permission(client, &properties.request.id).await?;
            *last_activity = tokio::time::Instant::now();
            Ok(false)
        }
        Event::QuestionAsked { properties } => {
            reply_question(client, &properties.request).await?;
            *last_activity = tokio::time::Instant::now();
            Ok(false)
        }
        Event::MessagePartUpdated { .. }
        | Event::MessagePartDelta { .. }
        | Event::MessageUpdated { .. } => {
            *last_activity = tokio::time::Instant::now();
            Ok(false)
        }
        _ => Ok(false),
    }
}

async fn handle_pending_interactions(client: &Client, session_id: &str) -> Result<bool> {
    let permissions = client
        .permissions()
        .list()
        .await
        .unwrap_or_else(|error| {
            warn!(session_id, %error, "failed to list permissions");
            Vec::new()
        });

    if let Some(permission) = permissions
        .into_iter()
        .find(|request| request.session_id == session_id)
    {
        reply_permission(client, &permission.id).await?;
        return Ok(true);
    }

    let questions = client
        .question()
        .list()
        .await
        .unwrap_or_else(|error| {
            warn!(session_id, %error, "failed to list questions");
            Vec::new()
        });

    if let Some(question) = questions
        .into_iter()
        .find(|request| request.session_id == session_id)
    {
        reply_question(client, &question).await?;
        return Ok(true);
    }

    Ok(false)
}

async fn reply_permission(client: &Client, request_id: &str) -> Result<()> {
    client
        .permissions()
        .reply(
            request_id,
            &PermissionReplyRequest {
                reply: PermissionReply::Always,
                message: None,
            },
        )
        .await
        .with_context(|| format!("reply to permission request {request_id}"))?;

    debug!(request_id, "auto-approved permission");
    Ok(())
}

async fn reply_question(
    client: &Client,
    question: &opencode_rs::types::question::QuestionRequest,
) -> Result<()> {
    let answers = question
        .questions
        .iter()
        .map(|info| {
            if let Some(option) = info.options.first() {
                vec![option.label.clone()]
            } else {
                vec![DEFAULT_QUESTION_ANSWER.to_string()]
            }
        })
        .collect();

    client
        .question()
        .reply(
            &question.id,
            &QuestionReply {
                answers,
            },
        )
        .await
        .with_context(|| format!("reply to question request {}", question.id))?;

    debug!(request_id = %question.id, "auto-answered question");
    Ok(())
}

fn verify_output(path: &Path, phase: &str) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("expected output for {phase} at {}", path.display()))?;

    if metadata.len() == 0 {
        bail!("output for {phase} is empty: {}", path.display());
    }

    Ok(())
}
