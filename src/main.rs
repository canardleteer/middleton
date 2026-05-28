mod agent;
mod claude_agent;
mod manifest;
mod opencode;
mod pdf;
mod prompts;
mod session;
mod target;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::agent::AgentKind;
use crate::claude_agent::{
    ClaudeCodeRuntime, model_label as claude_model_label, parse_model as claude_model,
};
use crate::manifest::SessionManifest;
use crate::opencode::{
    OpenCodeRuntime, ensure_opencode_go_api_key, model_ref_label, opencode_go_model, start_runtime,
    stop_runtime,
};
use crate::prompts::{
    DEFENSE_BUILD, DEFENSE_PROMPT, DEPTH_BUILD, DEPTH_PROMPT, INTENT_BUILD, INTENT_PROMPT,
    JUDGEMENT_BUILD, JUDGEMENT_PROMPT, PROSECUTION_BUILD, PROSECUTION_PROMPT, with_note,
};
use crate::session::run_plan_build_phase as run_opencode_plan_build_phase;

#[derive(Parser)]
#[command(
    name = "middleton",
    about = "Run prosecution/defense/judgement review via OpenCode or Claude Code"
)]
struct Cli {
    /// Local directory or git repository URL
    #[arg(required_unless_present = "export_pdf")]
    input: Option<String>,

    /// Clone/output directory when input is a git URL (default: ./<repo-name> in CWD)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Export markdown files in a `.middleton` directory to PDF, skipping files that
    /// already have a matching `.pdf`
    #[arg(long, value_name = "DIR")]
    export_pdf: Option<PathBuf>,

    /// Agent backend to run analysis phases
    #[arg(long, value_enum, default_value_t = AgentKind::OpenCode)]
    agent: AgentKind,

    /// Model id for the selected agent (OpenCode Go catalog id, or sonnet/opus/haiku for Claude Code)
    #[arg(long, default_value = "kimi-k2.5")]
    model: String,

    /// OpenCode server bind hostname
    #[arg(long, default_value = "127.0.0.1")]
    hostname: String,

    /// OpenCode binary path
    #[arg(long, default_value = "opencode")]
    opencode: String,

    /// Claude Code binary path
    #[arg(long, default_value = "claude")]
    claude: String,

    /// Log level filter (RUST_LOG-style; default info)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Pandoc binary path for PDF export
    #[arg(long, default_value = "pandoc")]
    pandoc: String,

    /// Skip pandoc PDF export at the end
    #[arg(long)]
    skip_pdf: bool,

    /// Additional context about the artifact under review, prepended to all analysis prompts
    #[arg(long)]
    note: Option<String>,
}

enum AgentRuntime {
    OpenCode(Box<OpenCodeRuntime>),
    ClaudeCode(ClaudeCodeRuntime),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(&cli)?;

    if let Some(middleton_dir) = &cli.export_pdf {
        return run_export_pdf(middleton_dir, &cli.pandoc);
    }

    let input = cli
        .input
        .as_deref()
        .context("input path or git URL is required unless --export-pdf is used")?;

    if cli.agent == AgentKind::OpenCode {
        ensure_opencode_go_api_key()?;
    }

    let target = target::resolve_target(input, &cli.output)?;
    std::fs::create_dir_all(target.join(".middleton"))
        .with_context(|| format!("create {}", target.join(".middleton").display()))?;

    info!(
        agent = cli.agent.label(),
        model = %cli.model,
        target = %target.display(),
        has_note = cli.note.as_ref().is_some_and(|note| !note.trim().is_empty()),
        "starting middleton"
    );

    let runtime = start_agent_runtime(&target, &cli).await?;
    let mut manifest = SessionManifest::load_or_default(&target)?;

    let pipeline_result = run_pipeline(
        &runtime,
        &target,
        &cli.model,
        &mut manifest,
        cli.note.as_deref(),
    )
    .await;

    if pipeline_result.is_err() {
        let _ = manifest.save(&target);
    }

    stop_agent_runtime(runtime).await?;

    pipeline_result?;
    if !cli.skip_pdf {
        let middleton_dir = target.join(".middleton");
        let pdfs = pdf::export_markdown_pdfs(&middleton_dir, &cli.pandoc)?;
        info!(count = pdfs.len(), dir = %middleton_dir.display(), "pdf export complete");
    }
    info!(
        target = %target.display(),
        manifest_path = %target.join(".middleton/sessions.json").display(),
        "middleton complete"
    );
    Ok(())
}

async fn start_agent_runtime(target: &Path, cli: &Cli) -> Result<AgentRuntime> {
    match cli.agent {
        AgentKind::OpenCode => {
            let runtime = start_runtime(target, &cli.hostname, &cli.opencode).await?;
            info!(
                provider_model = %model_ref_label(&opencode_go_model(&cli.model)),
                "OpenCode server started"
            );
            Ok(AgentRuntime::OpenCode(Box::new(runtime)))
        }
        AgentKind::ClaudeCode => {
            let runtime = claude_agent::start_runtime(&cli.claude).await?;
            info!(
                provider_model = claude_model_label(claude_model(&cli.model)),
                "Claude Code client ready"
            );
            Ok(AgentRuntime::ClaudeCode(runtime))
        }
    }
}

async fn stop_agent_runtime(runtime: AgentRuntime) -> Result<()> {
    match runtime {
        AgentRuntime::OpenCode(runtime) => {
            stop_runtime(runtime.server).await?;
            info!("OpenCode server stopped");
        }
        AgentRuntime::ClaudeCode(_) => {
            info!("Claude Code sessions complete");
        }
    }
    Ok(())
}

fn run_export_pdf(middleton_dir: &Path, pandoc: &str) -> Result<()> {
    if !middleton_dir.is_dir() {
        bail!(
            "--export-pdf path is not a directory: {}",
            middleton_dir.display()
        );
    }

    info!(dir = %middleton_dir.display(), "exporting missing markdown pdfs");
    let pdfs = pdf::export_missing_markdown_pdfs(middleton_dir, pandoc)?;
    info!(
        count = pdfs.len(),
        dir = %middleton_dir.display(),
        "pdf export complete"
    );
    Ok(())
}

async fn run_pipeline(
    runtime: &AgentRuntime,
    target: &Path,
    model: &str,
    manifest: &mut SessionManifest,
    note: Option<&str>,
) -> Result<()> {
    let intent_plan = with_note(INTENT_PROMPT, note);
    let intent_build = with_note(INTENT_BUILD, note);
    let depth_plan = with_note(DEPTH_PROMPT, note);
    let depth_build = with_note(DEPTH_BUILD, note);
    let prosecution_plan = with_note(PROSECUTION_PROMPT, note);
    let prosecution_build = with_note(PROSECUTION_BUILD, note);
    let defense_plan = with_note(DEFENSE_PROMPT, note);
    let defense_build = with_note(DEFENSE_BUILD, note);
    let judgement_plan = with_note(JUDGEMENT_PROMPT, note);
    let judgement_build = with_note(JUDGEMENT_BUILD, note);

    let intent_scan_1 = target.join(".middleton/INTENT-SCAN-1.md");
    let intent_scan_2 = target.join(".middleton/INTENT-SCAN-2.md");
    let prosecution_output = target.join(".middleton/PROSECUTION.md");
    let depth_output = target.join(".middleton/DEPTH.md");
    let defense_output = target.join(".middleton/DEFENSE.md");
    let judgement_output = target.join(".middleton/JUDGEMENT.md");

    let intent_outputs = [intent_scan_1.as_path(), intent_scan_2.as_path()];
    let depth_outputs = [depth_output.as_path()];

    let (intent_id, depth_id) = tokio::try_join!(
        run_phase(
            runtime,
            target,
            model,
            "intent",
            &intent_plan,
            &intent_build,
            &intent_outputs,
        ),
        run_phase(
            runtime,
            target,
            model,
            "depth",
            &depth_plan,
            &depth_build,
            &depth_outputs,
        ),
    )?;

    manifest.set("intent", intent_id);
    manifest.set("depth", depth_id);
    manifest.save(target)?;

    let prosecution_outputs = [prosecution_output.as_path()];
    let prosecution_id = run_phase(
        runtime,
        target,
        model,
        "prosecution",
        &prosecution_plan,
        &prosecution_build,
        &prosecution_outputs,
    )
    .await?;
    manifest.set("prosecution", prosecution_id);
    manifest.save(target)?;

    let defense_outputs = [defense_output.as_path()];
    let defense_id = run_phase(
        runtime,
        target,
        model,
        "defense",
        &defense_plan,
        &defense_build,
        &defense_outputs,
    )
    .await?;
    manifest.set("defense", defense_id);
    manifest.save(target)?;

    let judgement_outputs = [judgement_output.as_path()];
    let judgement_id = run_phase(
        runtime,
        target,
        model,
        "judgement",
        &judgement_plan,
        &judgement_build,
        &judgement_outputs,
    )
    .await?;
    manifest.set("judgement", judgement_id);
    manifest.save(target)?;

    Ok(())
}

async fn run_phase(
    runtime: &AgentRuntime,
    target: &Path,
    model: &str,
    phase: &str,
    plan_prompt: &str,
    build_prompt: &str,
    expected_outputs: &[&Path],
) -> Result<String> {
    match runtime {
        AgentRuntime::OpenCode(runtime) => {
            let model = opencode_go_model(model);
            run_opencode_plan_build_phase(
                &runtime.client,
                phase,
                plan_prompt,
                build_prompt,
                &model,
                expected_outputs,
            )
            .await
        }
        AgentRuntime::ClaudeCode(runtime) => {
            let model = claude_model(model);
            claude_agent::run_plan_build_phase(
                &runtime.client,
                phase,
                plan_prompt,
                build_prompt,
                model,
                target,
                expected_outputs,
            )
            .await
        }
    }
}

fn init_logging(cli: &Cli) -> Result<()> {
    let filter = if std::env::var_os("RUST_LOG").is_some() {
        EnvFilter::from_default_env()
    } else {
        EnvFilter::new(cli.log_level.as_str())
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .ok();

    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("off"))
        .try_init();

    Ok(())
}
