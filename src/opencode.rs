use std::path::Path;

use anyhow::{Context, Result};
use opencode_rs::Client;
use opencode_rs::ClientBuilder;
use opencode_rs::server::{ManagedServer, ServerOptions};
use opencode_rs::types::project::ModelRef;
use url::Url;

pub struct OpenCodeRuntime {
    pub server: ManagedServer,
    pub client: Client,
}

pub fn opencode_go_model(model_id: &str) -> ModelRef {
    let model_id = model_id.strip_prefix("opencode-go/").unwrap_or(model_id);
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

pub async fn start_runtime(
    target: &Path,
    hostname: &str,
    opencode_bin: &str,
) -> Result<OpenCodeRuntime> {
    let server = ManagedServer::start(
        ServerOptions::new()
            .directory(target)
            .hostname(hostname)
            .binary(opencode_bin),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_bare_and_provider_qualified_go_models() {
        for model in ["minimax-m3.1-flash", "opencode-go/minimax-m3.1-flash"] {
            let model = opencode_go_model(model);
            assert_eq!(model.provider_id.as_deref(), Some("opencode-go"));
            assert_eq!(model.model_id.as_deref(), Some("minimax-m3.1-flash"));
        }
    }
}
