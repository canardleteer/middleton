use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::export_common::format_report_title;

const TRIAL_FILENAME: &str = "TRIAL.md";

/// Primary trial briefs in the order they appear in the consolidated record.
const PRIMARY_SECTIONS: &[&str] = &["JUDGEMENT.md", "PROSECUTION.md", "DEFENSE.md", "DEPTH.md"];

pub fn trial_markdown_path(dir: &Path) -> PathBuf {
    dir.join(TRIAL_FILENAME)
}

pub fn is_trial_source(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(TRIAL_FILENAME))
}

/// Merge phase markdown into a single `TRIAL.md` without removing the source files.
pub fn compile(dir: &Path) -> Result<Option<PathBuf>> {
    let mut body = String::from(
        "# Middleton Trial Record\n\n\
This document consolidates the middleton analysis artifacts. \
Individual reports remain in this directory.\n",
    );
    let mut sections = 0usize;

    for filename in PRIMARY_SECTIONS {
        if append_file_section(&mut body, dir, filename)? {
            sections += 1;
        }
    }

    for path in other_markdown_files(dir)? {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if append_file_section(&mut body, dir, filename)? {
            sections += 1;
        }
    }

    if sections == 0 {
        warn!(dir = %dir.display(), "no markdown artifacts found; skipping TRIAL.md");
        return Ok(None);
    }

    let trial_path = trial_markdown_path(dir);
    fs::write(&trial_path, body.trim_end())
        .with_context(|| format!("write {}", trial_path.display()))?;
    info!(
        path = %trial_path.display(),
        sections,
        "compiled trial record"
    );
    Ok(Some(trial_path))
}

fn append_file_section(body: &mut String, dir: &Path, filename: &str) -> Result<bool> {
    let path = dir.join(filename);
    if !path.is_file() {
        warn!(file = filename, "skipping missing trial section");
        return Ok(false);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?
        .trim()
        .to_string();

    if content.is_empty() {
        warn!(file = filename, "skipping empty trial section");
        return Ok(false);
    }

    let title = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(format_report_title)
        .unwrap_or_else(|| filename.to_string());

    body.push_str("\n\n---\n\n");
    body.push_str(&format!("# {title}\n\n{content}\n"));
    Ok(true)
}

fn other_markdown_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = fs::read_dir(dir)
        .with_context(|| format!("read directory {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        })
        .filter(|path| !is_trial_source(path))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| !PRIMARY_SECTIONS.contains(&name))
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_orders_sections() {
        let dir = std::env::temp_dir().join(format!("middleton-trial-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::write(dir.join("INTENT-SCAN-1.md"), "# Intent one").unwrap();
        fs::write(dir.join("DEPTH.md"), "# Depth body").unwrap();
        fs::write(dir.join("JUDGEMENT.md"), "# Judgement body").unwrap();
        fs::write(dir.join("PROSECUTION.md"), "# Prosecution body").unwrap();

        let trial = compile(&dir).unwrap().expect("trial path");
        let text = fs::read_to_string(&trial).unwrap();

        let judgement = text.find("# Judgement").expect("judgement heading");
        let prosecution = text.find("# Prosecution").expect("prosecution heading");
        let depth = text.find("# Depth").expect("depth heading");
        let intent = text.find("# Intent Scan 1").expect("intent heading");

        assert!(judgement < prosecution);
        assert!(prosecution < depth);
        assert!(depth < intent);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_trial_source_matches_trial_md() {
        assert!(is_trial_source(Path::new("/tmp/TRIAL.md")));
        assert!(!is_trial_source(Path::new("/tmp/JUDGEMENT.md")));
    }
}
