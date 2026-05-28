mod manifest;
mod opencode;
mod pdf;
mod prompts;
mod session;
mod target;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::manifest::SessionManifest;
use crate::opencode::{
    ensure_opencode_go_api_key, model_ref_label, opencode_go_model, start_runtime, stop_runtime,
};
use crate::prompts::{
    DEFENSE_BUILD, DEFENSE_PROMPT, DEPTH_BUILD, DEPTH_PROMPT, INTENT_BUILD, INTENT_PROMPT,
    JUDGEMENT_BUILD, JUDGEMENT_PROMPT, PROSECUTION_BUILD, PROSECUTION_PROMPT,
};
use crate::session::run_plan_build_phase;

#[derive(Parser)]
#[command(name = "middleton", about = "Run prosecution/defense/judgement review via OpenCode")]
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

    /// OpenCode Go catalog model id (default: kimi-k2.5). Not a Zen opencode/... ref.
    #[arg(long, default_value = "kimi-k2.5")]
    model: String,

    /// OpenCode server bind hostname
    #[arg(long, default_value = "127.0.0.1")]
    hostname: String,

    /// OpenCode binary path
    #[arg(long, default_value = "opencode")]
    opencode: String,

    /// Log level filter (RUST_LOG-style; default info)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Pandoc binary path for PDF export
    #[arg(long, default_value = "pandoc")]
    pandoc: String,

    /// Skip pandoc PDF export at the end
    #[arg(long)]
    skip_pdf: bool,
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

    ensure_opencode_go_api_key()?;
    let target = target::resolve_target(input, &cli.output)?;
    std::fs::create_dir_all(target.join(".middleton"))
        .with_context(|| format!("create {}", target.join(".middleton").display()))?;

    let model = opencode_go_model(&cli.model);
    info!(
        provider_model = %model_ref_label(&model),
        target = %target.display(),
        "starting middleton"
    );

    let runtime = start_runtime(&target, &cli).await?;
    let mut manifest = SessionManifest::load_or_default(&target)?;

    let pipeline_result = run_pipeline(
        &runtime.client,
        &target,
        &model,
        &mut manifest,
    )
    .await;

    if pipeline_result.is_err() {
        let _ = manifest.save(&target);
    }

    stop_runtime(runtime.server).await?;
    info!("OpenCode server stopped");

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

fn run_export_pdf(middleton_dir: &PathBuf, pandoc: &str) -> Result<()> {
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
    client: &opencode_rs::Client,
    target: &PathBuf,
    model: &opencode_rs::types::project::ModelRef,
    manifest: &mut SessionManifest,
) -> Result<()> {
    let intent_scan_1 = target.join(".middleton/INTENT-SCAN-1.md");
    let intent_scan_2 = target.join(".middleton/INTENT-SCAN-2.md");
    let prosecution_output = target.join(".middleton/PROSECUTION.md");
    let depth_output = target.join(".middleton/DEPTH.md");
    let defense_output = target.join(".middleton/DEFENSE.md");
    let judgement_output = target.join(".middleton/JUDGEMENT.md");

    let intent_outputs = [intent_scan_1.as_path(), intent_scan_2.as_path()];
    let depth_outputs = [depth_output.as_path()];

    let (intent_id, depth_id) = tokio::try_join!(
        run_plan_build_phase(
            client,
            "intent",
            INTENT_PROMPT,
            INTENT_BUILD,
            model,
            &intent_outputs,
        ),
        run_plan_build_phase(
            client,
            "depth",
            DEPTH_PROMPT,
            DEPTH_BUILD,
            model,
            &depth_outputs,
        ),
    )?;

    manifest.set("intent", intent_id);
    manifest.set("depth", depth_id);
    manifest.save(target)?;

    let prosecution_outputs = [prosecution_output.as_path()];
    let prosecution_id = run_plan_build_phase(
        client,
        "prosecution",
        PROSECUTION_PROMPT,
        PROSECUTION_BUILD,
        model,
        &prosecution_outputs,
    )
    .await?;
    manifest.set("prosecution", prosecution_id);
    manifest.save(target)?;

    let defense_outputs = [defense_output.as_path()];
    let defense_id = run_plan_build_phase(
        client,
        "defense",
        DEFENSE_PROMPT,
        DEFENSE_BUILD,
        model,
        &defense_outputs,
    )
    .await?;
    manifest.set("defense", defense_id);
    manifest.save(target)?;

    let judgement_outputs = [judgement_output.as_path()];
    let judgement_id = run_plan_build_phase(
        client,
        "judgement",
        JUDGEMENT_PROMPT,
        JUDGEMENT_BUILD,
        model,
        &judgement_outputs,
    )
    .await?;
    manifest.set("judgement", judgement_id);
    manifest.save(target)?;

    Ok(())
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
