use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use tracing::{info, warn};

const OUTPUT_SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn missing_outputs(expected_outputs: &[&Path]) -> Vec<PathBuf> {
    expected_outputs
        .iter()
        .filter(|path| !is_valid_output(path))
        .map(|path| (*path).to_path_buf())
        .collect()
}

pub fn is_valid_output(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.len() > 0)
        .unwrap_or(false)
}

pub fn verify_outputs(expected_outputs: &[&Path], phase: &str) -> Result<()> {
    let missing = missing_outputs(expected_outputs);
    if missing.is_empty() {
        return Ok(());
    }

    bail!("missing output for {phase}: {}", format_paths(&missing));
}

pub async fn wait_for_outputs(expected_outputs: &[&Path], timeout: Duration) -> bool {
    if expected_outputs.is_empty() {
        return true;
    }

    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if missing_outputs(expected_outputs).is_empty() {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    missing_outputs(expected_outputs).is_empty()
}

pub async fn settle_outputs(expected_outputs: &[&Path]) -> bool {
    wait_for_outputs(expected_outputs, OUTPUT_SETTLE_TIMEOUT).await
}

pub fn nudge_prompt(missing: &[PathBuf]) -> String {
    format!(
        "The following required markdown file(s) are still missing or empty:\n{}\n\n\
Write every missing file now at the exact path(s) above using your completed plan. \
All required outputs for this phase must exist and be non-empty before you stop. \
Do not modify any other files.",
        missing
            .iter()
            .map(|path| format!("- `{}`", path.display()))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

pub fn format_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn log_missing_outputs(phase: &str, attempt: u32, max_attempts: u32, missing: &[PathBuf]) {
    warn!(
        phase,
        attempt,
        max_attempts,
        missing = %format_paths(missing),
        "required markdown outputs still missing"
    );
}

pub fn log_outputs_ready(phase: &str, attempt: u32) {
    if attempt > 1 {
        info!(phase, attempt, "all required markdown outputs present");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_missing_and_empty_outputs() {
        let dir =
            std::env::temp_dir().join(format!("middleton-output-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let present = dir.join("present.md");
        let empty = dir.join("empty.md");
        let missing = dir.join("missing.md");
        fs::write(&present, "content").unwrap();
        fs::write(&empty, "").unwrap();

        let expected = [present.as_path(), empty.as_path(), missing.as_path()];
        let still_missing = missing_outputs(&expected);
        assert_eq!(still_missing.len(), 2);
        assert!(still_missing.iter().any(|path| path.ends_with("empty.md")));
        assert!(
            still_missing
                .iter()
                .any(|path| path.ends_with("missing.md"))
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nudge_prompt_lists_paths() {
        let prompt = nudge_prompt(&[PathBuf::from("/repo/.middleton/DEPTH.md")]);
        assert!(prompt.contains("DEPTH.md"));
        assert!(prompt.contains("still missing"));
    }
}
