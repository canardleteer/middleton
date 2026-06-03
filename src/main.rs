mod agent;
#[allow(clippy::too_many_arguments)]
mod claude_agent;
#[allow(clippy::too_many_arguments)]
mod codex_agent;
mod epub;
mod export_common;
mod git_scope;
mod manifest;
mod opencode;
mod output;
mod paths;
mod pdf;
mod prompts;
mod run_log;
mod session;
mod target;
mod trial;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::agent::{AgentKind, ReviewProfile};
use crate::claude_agent::{
    ClaudeCodeRuntime, model_label as claude_model_label, parse_model as claude_model,
};
use crate::codex_agent::{
    CodexRuntime, model_label as codex_model_label, parse_model as codex_model,
};
use crate::manifest::SessionManifest;
use crate::opencode::{
    OpenCodeRuntime, ensure_opencode_go_api_key, model_ref_label, opencode_go_model, start_runtime,
    stop_runtime,
};
use crate::paths::ArtifactPaths;
use crate::prompts::{PhasePrompts, with_note};
use crate::run_log::{RunLog, RunOptions};
use crate::session::run_plan_build_phase as run_opencode_plan_build_phase;

#[derive(Parser)]
#[command(
    name = "middleton",
    about = "Run prosecution/defense/judgement review via OpenCode, Claude, or Codex"
)]
struct Cli {
    /// Local directory or git repository URL
    #[arg(required_unless_present_any = ["export_pdf", "export_epub"])]
    input: Option<String>,

    /// Clone/output directory when input is a git URL (default: ./<repo-name> in CWD)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Export markdown files in a Middleton run directory to PDF, skipping files that
    /// already have a matching `.pdf`
    #[arg(long, value_name = "DIR")]
    export_pdf: Option<PathBuf>,

    /// Export markdown files in a Middleton run directory to EPUB, skipping files that
    /// already have a matching `.epub`
    #[arg(long, value_name = "DIR")]
    export_epub: Option<PathBuf>,

    /// Agent backend to run analysis phases
    #[arg(long, value_enum, default_value_t = AgentKind::OpenCode)]
    agent: AgentKind,

    /// Model id for the selected agent (OpenCode Go catalog id, sonnet/opus/haiku for Claude, or a Codex model id)
    #[arg(long, default_value = "kimi-k2.5")]
    model: String,

    /// OpenCode server bind hostname
    #[arg(long, default_value = "127.0.0.1")]
    hostname: String,

    /// OpenCode binary path
    #[arg(long, default_value = "opencode")]
    opencode: String,

    /// Claude binary path
    #[arg(long, default_value = "claude")]
    claude: String,

    /// Codex CLI binary path
    #[arg(long, default_value = "codex")]
    codex: String,

    /// Log level filter (RUST_LOG-style; default info)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Pandoc binary path for PDF and EPUB export
    #[arg(long, default_value = "pandoc")]
    pandoc: String,

    /// Skip pandoc PDF export at the end
    #[arg(long)]
    skip_pdf: bool,

    /// Skip pandoc EPUB export at the end
    #[arg(long)]
    skip_epub: bool,

    /// Additional context about the artifact under review, prepended to all analysis prompts
    #[arg(long)]
    note: Option<String>,

    /// Corpus lens: repository (code + docs) or documents (spec/design packs, no code penalty)
    #[arg(long, value_enum, default_value_t = ReviewProfile::Repository)]
    profile: ReviewProfile,
}

enum AgentRuntime {
    OpenCode(Box<OpenCodeRuntime>),
    ClaudeCode(ClaudeCodeRuntime),
    Codex(CodexRuntime),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(&cli)?;

    if let Some(middleton_dir) = &cli.export_pdf {
        return run_export_pdf(middleton_dir, &cli.pandoc);
    }

    if let Some(middleton_dir) = &cli.export_epub {
        return run_export_epub(middleton_dir, &cli.pandoc);
    }

    let input = cli.input.as_deref().context(
        "input path or git URL is required unless --export-pdf or --export-epub is used",
    )?;

    if cli.agent == AgentKind::OpenCode {
        ensure_opencode_go_api_key()?;
    }

    let target = target::resolve_target(input, &cli.output)?;
    let artifacts = ArtifactPaths::new(&target, cli.agent, &cli.model);
    artifacts
        .ensure_dir()
        .with_context(|| format!("create {}", artifacts.dir.display()))?;

    persist_reviewer_note(&artifacts, cli.note.as_deref())?;

    let run_log = Arc::new(RunLog::open(&artifacts)?);
    run_log.record_run_start(&run_options_from_cli(&cli, &target, input))?;

    info!(
        agent = cli.agent.label(),
        profile = cli.profile.label(),
        model = %cli.model,
        target = %target.display(),
        artifacts_dir = %artifacts.dir.display(),
        actions_log = %run_log.path().display(),
        has_note = cli.note.as_ref().is_some_and(|note| !note.trim().is_empty()),
        "starting middleton"
    );

    let runtime = start_agent_runtime(&target, &cli).await?;
    let mut manifest = SessionManifest::load_or_default(&artifacts)?;

    let pipeline_result = run_pipeline(
        &runtime,
        &artifacts,
        &cli.model,
        cli.profile,
        &mut manifest,
        cli.note.as_deref(),
        Arc::clone(&run_log),
    )
    .await;

    if pipeline_result.is_err() {
        let _ = manifest.save(&artifacts);
    }

    stop_agent_runtime(runtime).await?;

    pipeline_result?;

    let trial_md = trial::compile(&artifacts.dir)?;

    if !cli.skip_pdf {
        let pdfs = pdf::export_markdown_pdfs(&artifacts.dir, &cli.pandoc)?;
        info!(
            count = pdfs.len(),
            dir = %artifacts.dir.display(),
            "phase pdf export complete"
        );
        if let Some(ref trial_md) = trial_md {
            let trial_pdf = pdf::export_markdown_file(&artifacts.dir, &cli.pandoc, trial_md)?;
            info!(
                markdown = %trial_md.display(),
                pdf = %trial_pdf.display(),
                "trial pdf export complete"
            );
        }
    }

    if !cli.skip_epub {
        let epubs = epub::export_markdown_epubs(&artifacts.dir, &cli.pandoc)?;
        info!(
            count = epubs.len(),
            dir = %artifacts.dir.display(),
            "phase epub export complete"
        );
        if let Some(ref trial_md) = trial_md {
            let trial_epub = epub::export_markdown_file(&artifacts.dir, &cli.pandoc, trial_md)?;
            info!(
                markdown = %trial_md.display(),
                epub = %trial_epub.display(),
                "trial epub export complete"
            );
        }
    }
    git_scope::warn_if_middleton_not_gitignored(&target);

    info!(
        target = %target.display(),
        manifest_path = %artifacts.join("sessions.json").display(),
        trial = trial_md
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "not created".to_string()),
        "middleton complete"
    );
    Ok(())
}

fn persist_reviewer_note(artifacts: &ArtifactPaths, note: Option<&str>) -> Result<()> {
    let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) else {
        return Ok(());
    };

    let context_dir = artifacts.join("context");
    std::fs::create_dir_all(&context_dir)
        .with_context(|| format!("create reviewer note directory {}", context_dir.display()))?;
    let note_path = context_dir.join("reviewer-note.md");
    let contents = format!(
        "# Private reviewer context\n\n\
This note was supplied by the person requesting the analysis. It is not part of the \
public trial record.\n\n\
{note}\n"
    );
    std::fs::write(&note_path, contents)
        .with_context(|| format!("write reviewer note {}", note_path.display()))?;
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
                "Claude client ready"
            );
            Ok(AgentRuntime::ClaudeCode(runtime))
        }
        AgentKind::Codex => {
            let runtime = codex_agent::start_runtime(&cli.codex, target).await?;
            info!(
                provider_model = codex_model_label(codex_model(&cli.model).as_deref()),
                "Codex app-server ready"
            );
            Ok(AgentRuntime::Codex(runtime))
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
            info!("Claude sessions complete");
        }
        AgentRuntime::Codex(runtime) => {
            codex_agent::stop_runtime(runtime).await?;
            info!("Codex app-server stopped");
        }
    }
    Ok(())
}

fn run_export_epub(middleton_dir: &Path, pandoc: &str) -> Result<()> {
    if !middleton_dir.is_dir() {
        bail!(
            "--export-epub path is not a directory: {}",
            middleton_dir.display()
        );
    }

    info!(dir = %middleton_dir.display(), "exporting missing markdown epubs");
    let trial_md = trial::compile(middleton_dir)?;
    let epubs = epub::export_missing_markdown_epubs(middleton_dir, pandoc)?;
    if let Some(trial_md) = trial_md {
        let trial_epub = trial::trial_markdown_path(middleton_dir).with_extension("epub");
        if !trial_epub.exists() {
            let exported = epub::export_markdown_file(middleton_dir, pandoc, &trial_md)?;
            info!(
                markdown = %trial_md.display(),
                epub = %exported.display(),
                "trial epub export complete"
            );
        }
    }
    info!(
        count = epubs.len(),
        dir = %middleton_dir.display(),
        "epub export complete"
    );
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
    let trial_md = trial::compile(middleton_dir)?;
    let pdfs = pdf::export_missing_markdown_pdfs(middleton_dir, pandoc)?;
    if let Some(trial_md) = trial_md {
        let trial_pdf = trial::trial_markdown_path(middleton_dir).with_extension("pdf");
        if !trial_pdf.exists() {
            let exported = pdf::export_markdown_file(middleton_dir, pandoc, &trial_md)?;
            info!(
                markdown = %trial_md.display(),
                pdf = %exported.display(),
                "trial pdf export complete"
            );
        }
    }
    info!(
        count = pdfs.len(),
        dir = %middleton_dir.display(),
        "pdf export complete"
    );
    Ok(())
}

fn run_options_from_cli(cli: &Cli, target: &Path, input: &str) -> RunOptions {
    RunOptions {
        input: input.to_string(),
        target: target.to_path_buf(),
        agent: cli.agent,
        profile: cli.profile,
        model: cli.model.clone(),
        hostname: cli.hostname.clone(),
        opencode_bin: cli.opencode.clone(),
        claude_bin: cli.claude.clone(),
        codex_bin: cli.codex.clone(),
        skip_pdf: cli.skip_pdf,
        skip_epub: cli.skip_epub,
        note_present: cli
            .note
            .as_ref()
            .is_some_and(|note| !note.trim().is_empty()),
    }
}

async fn run_pipeline(
    runtime: &AgentRuntime,
    artifacts: &ArtifactPaths,
    model: &str,
    profile: ReviewProfile,
    manifest: &mut SessionManifest,
    note: Option<&str>,
    run_log: Arc<RunLog>,
) -> Result<()> {
    let prompts = PhasePrompts::new(artifacts, profile);
    let intent_plan = with_note(&prompts.intent_plan, note);
    let intent_build = with_note(&prompts.intent_build, note);
    let depth_plan = with_note(&prompts.depth_plan, note);
    let depth_build = with_note(&prompts.depth_build, note);
    let prosecution_plan = with_note(&prompts.prosecution_plan, note);
    let prosecution_build = with_note(&prompts.prosecution_build, note);
    let defense_plan = with_note(&prompts.defense_plan, note);
    let defense_build = with_note(&prompts.defense_build, note);
    let judgement_plan = with_note(&prompts.judgement_plan, note);
    let judgement_build = with_note(&prompts.judgement_build, note);

    let intent_scan_1 = artifacts.join("INTENT-SCAN-1.md");
    let intent_scan_2 = artifacts.join("INTENT-SCAN-2.md");
    let prosecution_output = artifacts.join("PROSECUTION.md");
    let depth_output = artifacts.join("DEPTH.md");
    let defense_output = artifacts.join("DEFENSE.md");
    let judgement_output = artifacts.join("JUDGEMENT.md");

    let intent_outputs = [intent_scan_1.as_path(), intent_scan_2.as_path()];
    let depth_outputs = [depth_output.as_path()];

    let (intent_id, depth_id) = tokio::try_join!(
        run_phase(
            runtime,
            artifacts,
            model,
            profile,
            "intent",
            &intent_plan,
            &intent_build,
            &intent_outputs,
            Arc::clone(&run_log),
        ),
        run_phase(
            runtime,
            artifacts,
            model,
            profile,
            "depth",
            &depth_plan,
            &depth_build,
            &depth_outputs,
            Arc::clone(&run_log),
        ),
    )?;

    manifest.set("intent", intent_id);
    manifest.set("depth", depth_id);
    manifest.save(artifacts)?;

    let prosecution_outputs = [prosecution_output.as_path()];
    let prosecution_id = run_phase(
        runtime,
        artifacts,
        model,
        profile,
        "prosecution",
        &prosecution_plan,
        &prosecution_build,
        &prosecution_outputs,
        Arc::clone(&run_log),
    )
    .await?;
    manifest.set("prosecution", prosecution_id);
    manifest.save(artifacts)?;

    let defense_outputs = [defense_output.as_path()];
    let defense_id = run_phase(
        runtime,
        artifacts,
        model,
        profile,
        "defense",
        &defense_plan,
        &defense_build,
        &defense_outputs,
        Arc::clone(&run_log),
    )
    .await?;
    manifest.set("defense", defense_id);
    manifest.save(artifacts)?;

    let judgement_outputs = [judgement_output.as_path()];
    let judgement_id = run_phase(
        runtime,
        artifacts,
        model,
        profile,
        "judgement",
        &judgement_plan,
        &judgement_build,
        &judgement_outputs,
        Arc::clone(&run_log),
    )
    .await?;
    manifest.set("judgement", judgement_id);
    manifest.save(artifacts)?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_phase(
    runtime: &AgentRuntime,
    artifacts: &ArtifactPaths,
    model: &str,
    profile: ReviewProfile,
    phase: &str,
    plan_prompt: &str,
    build_prompt: &str,
    expected_outputs: &[&Path],
    run_log: Arc<RunLog>,
) -> Result<String> {
    let target = &artifacts.target;
    let write_prefix = artifacts.rel_prefix.as_str();
    match runtime {
        AgentRuntime::OpenCode(runtime) => {
            let model = opencode_go_model(model);
            run_opencode_plan_build_phase(
                &runtime.client,
                phase,
                plan_prompt,
                build_prompt,
                &model,
                profile,
                write_prefix,
                expected_outputs,
                run_log,
            )
            .await
        }
        AgentRuntime::ClaudeCode(runtime) => {
            let model = claude_model(model);
            claude_agent::run_plan_build_phase(
                runtime,
                phase,
                plan_prompt,
                build_prompt,
                model,
                profile,
                target,
                write_prefix,
                expected_outputs,
                run_log,
            )
            .await
        }
        AgentRuntime::Codex(runtime) => {
            let model = codex_model(model);
            codex_agent::run_plan_build_phase(
                runtime,
                phase,
                plan_prompt,
                build_prompt,
                model.as_deref(),
                profile,
                target,
                write_prefix,
                expected_outputs,
                run_log,
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
