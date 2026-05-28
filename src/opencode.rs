use std::path::Path;

use anyhow::{Context, Result, bail};
use opencode_rs::Client;
use opencode_rs::ClientBuilder;
use opencode_rs::server::{ManagedServer, ServerOptions};
use opencode_rs::types::project::ModelRef;
use url::Url;

use crate::Cli;

pub struct OpenCodeRuntime {
    pub server: ManagedServer,
    pub client: Client,
}

pub fn ensure_opencode_go_api_key() -> Result<()> {
    if std::env::var_os("OPENCODE_API_KEY")
        .is_some_and(|value| !value.is_empty())
    {
        return Ok(());
    }

    bail!(
        "OPENCODE_API_KEY is not set. Middleton requires an OpenCode Go API key \
         (provider opencode-go, not OpenCode Zen opencode/...)."
    );
}

pub fn opencode_go_model(model_id: &str) -> ModelRef {
    ModelRef {
        provider_id: Some("opencode-go".to_string()),
        model_id: Some(model_id.to_string()),
        variant: None,
        extra: serde_json::Value::Null,
    }
}

pub fn model_ref_label(model: &ModelRef) -> String {
    format!(
        "{}/{}",
        model.provider_id.as_deref().unwrap_or("unknown"),
        model.model_id.as_deref().unwrap_or("unknown")
    )
}

pub async fn start_runtime(target: &Path, cli: &Cli) -> Result<OpenCodeRuntime> {
    ensure_opencode_go_api_key()?;

    let server = ManagedServer::start(
        ServerOptions::new()
            .directory(target)
            .hostname(&cli.hostname)
            .binary(&cli.opencode),
    )
    .await
    .context("start opencode serve")?;

    let client = build_client(server.url(), target)?;
    Ok(OpenCodeRuntime { server, client })
}

pub fn build_client(base_url: &Url, target: &Path) -> Result<Client> {
    ClientBuilder::new()
        .base_url(base_url.to_string())
        .directory(target.to_string_lossy())
        .timeout_secs(3600)
        .build()
        .context("build OpenCode client")
}

pub async fn stop_runtime(runtime: ManagedServer) -> Result<()> {
    runtime.stop().await.context("stop opencode serve")
}
