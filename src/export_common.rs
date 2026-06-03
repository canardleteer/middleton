use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;

use crate::trial;

static MCQ_BULLET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\*\s+\*\*([A-Z])\)\*\*\s*").expect("valid regex"));

static ATX_HEADING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<indent>\s*)(?P<hashes>#{1,6})\s+(?P<title>.+)$").expect("valid regex")
});

fn strip_leading_hashes_from_title(title: &str) -> String {
    let mut t = title.trim();
    while let Some(rest) = t.strip_prefix('#') {
        t = rest.trim_start();
    }
    t.trim().to_string()
}

pub fn ensure_pandoc(pandoc: &str) -> Result<()> {
    let output = Command::new(pandoc)
        .arg("--version")
        .output()
        .with_context(|| format!("run `{pandoc} --version`"))?;

    if output.status.success() {
        return Ok(());
    }

    bail!("`{pandoc}` is unavailable or failed. Install pandoc for export.");
}

pub fn collect_markdown_files(
    middleton_dir: &Path,
    skip_existing: bool,
    extension: &str,
) -> Result<Vec<PathBuf>> {
    let mut files = fs::read_dir(middleton_dir)
        .with_context(|| format!("read directory {}", middleton_dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                && !trial::is_trial_source(path)
        })
        .filter(|path| !skip_existing || !path.with_extension(extension).exists())
        .collect::<Vec<_>>();

    files.sort();
    Ok(files)
}

pub fn title_from_markdown_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(format_report_title)
        .unwrap_or_else(|| "Middleton Report".to_string())
}

pub fn format_report_title(stem: &str) -> String {
    stem.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            let mut chars = lower.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Replace control characters that break LaTeX/XML export with visible escapes.
pub fn sanitize_markdown_for_export(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\t' | '\n' | '\r' => out.push(ch),
            c if c.is_control() => out.push_str(&control_char_escape(c)),
            _ => out.push(ch),
        }
    }
    out
}

fn control_char_escape(ch: char) -> String {
    let code = ch as u32;
    if code <= 0xFF {
        format!("\\x{code:02x}")
    } else {
        format!("\\u{{{code:x}}}")
    }
}

/// Rewrite legacy quiz bullets `* **A)** …` to `A) …` for lettered lists in PDF/EPUB.
pub fn normalize_mcq_bullet_lines(input: &str) -> String {
    MCQ_BULLET_RE.replace_all(input, "$1) ").to_string()
}

/// Remove duplicate `#` markers agents copy from prompt templates (e.g. `## ## captured intent`).
pub fn normalize_atx_heading_titles(input: &str) -> String {
    rewrite_atx_heading_lines(input, |hashes, title| {
        let cleaned = strip_leading_hashes_from_title(title);
        if cleaned.is_empty() {
            None
        } else {
            Some(format!("{hashes} {cleaned}"))
        }
    })
}

/// Level-3+ ATX headings (`###`, `####`, …) become bold lines so pandoc `--number-sections`
/// does not emit TOC entries like `11.1.3### filename` (section number glued to `#` markers).
fn rewrite_atx_heading_lines(
    input: &str,
    mut rewrite: impl FnMut(&str, &str) -> Option<String>,
) -> String {
    let mut in_fence = false;
    let mut out = Vec::new();

    for line in input.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out.push(line.to_string());
            continue;
        }

        if !in_fence && let Some(caps) = ATX_HEADING_RE.captures(line) {
            let indent = caps.name("indent").map(|m| m.as_str()).unwrap_or("");
            let hashes = caps.name("hashes").map(|m| m.as_str()).unwrap_or("");
            let title = caps.name("title").map(|m| m.as_str()).unwrap_or("");
            if let Some(body) = rewrite(hashes, title) {
                out.push(format!("{indent}{body}"));
                continue;
            }
        }

        out.push(line.to_string());
    }

    out.join("\n")
}

pub fn flatten_deep_headings_for_export(input: &str) -> String {
    rewrite_atx_heading_lines(input, |hashes, title| {
        if hashes.len() < 3 {
            return None;
        }
        let cleaned = strip_leading_hashes_from_title(title);
        if cleaned.is_empty() {
            None
        } else {
            Some(format!("**{cleaned}**"))
        }
    })
}

pub fn prepare_markdown_for_pandoc(raw: &str) -> String {
    let sanitized = sanitize_markdown_for_export(raw);
    let titles_clean = normalize_atx_heading_titles(&sanitized);
    let headings_flat = flatten_deep_headings_for_export(&titles_clean);
    normalize_mcq_bullet_lines(&headings_flat)
}

pub fn write_prepared_markdown(markdown: &Path, prepared: &str) -> Result<PathBuf> {
    let prepared_path = markdown.with_extension("md.middleton-export");
    fs::write(&prepared_path, prepared)
        .with_context(|| format!("write prepared markdown {}", prepared_path.display()))?;
    Ok(prepared_path)
}

pub fn read_and_prepare_markdown(markdown: &Path) -> Result<PathBuf> {
    let raw = fs::read_to_string(markdown)
        .with_context(|| format!("read markdown {}", markdown.display()))?;
    let prepared = prepare_markdown_for_pandoc(&raw);
    write_prepared_markdown(markdown, &prepared)
}

const DEJAVU_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
];

pub fn existing_dejavu_font_paths() -> Vec<&'static str> {
    DEJAVU_FONT_PATHS
        .iter()
        .copied()
        .filter(|path| Path::new(path).is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_report_titles() {
        assert_eq!(format_report_title("INTENT-SCAN-1"), "Intent Scan 1");
        assert_eq!(format_report_title("JUDGEMENT"), "Judgement");
    }

    #[test]
    fn sanitizes_control_characters() {
        let input = "magic bytes (ELF `\u{7f}ELF`)";
        let sanitized = sanitize_markdown_for_export(input);
        assert!(!sanitized.contains('\u{7f}'));
        assert!(sanitized.contains(r"\x7f"));
        assert_eq!(sanitized, "magic bytes (ELF `\\x7fELF`)");
    }

    #[test]
    fn preserves_tabs_and_newlines_when_sanitizing() {
        let input = "line one\nline two\tindented";
        assert_eq!(sanitize_markdown_for_export(input), input);
    }

    #[test]
    fn prepare_markdown_preserves_horizontal_rules() {
        let input = "before\n---\nafter\n";
        let out = prepare_markdown_for_pandoc(input);
        assert!(out.contains("---"));
        assert!(!out.contains("~ ~ ~"));
    }

    #[test]
    fn normalize_mcq_bullets_legacy_lines() {
        let raw = "*   **A)** First\n*   **B)** Second\n";
        assert_eq!(normalize_mcq_bullet_lines(raw), "A) First\nB) Second\n");
    }

    #[test]
    fn normalize_atx_heading_strips_duplicate_markers() {
        let raw = "## ## captured intent\n### ### docs/foo.md\n";
        let out = normalize_atx_heading_titles(raw);
        assert!(out.contains("## captured intent"));
        assert!(out.contains("### docs/foo.md"));
        assert!(!out.contains("## ##"));
        assert!(!out.contains("### ###"));
    }

    #[test]
    fn flatten_deep_headings_turns_h3_into_bold() {
        let raw = "## Section\n### docs/verify_in_5_minutes.md\nbody\n";
        let out = flatten_deep_headings_for_export(raw);
        assert!(out.contains("## Section"));
        assert!(out.contains("**docs/verify_in_5_minutes.md**"));
        assert!(!out.contains("### docs"));
    }

    #[test]
    fn flatten_deep_headings_skips_fenced_code() {
        let raw = "```\n### not a heading\n```\n### real heading\n";
        let out = flatten_deep_headings_for_export(raw);
        assert!(out.contains("```\n### not a heading\n```"));
        assert!(out.contains("**real heading**"));
    }

    #[test]
    fn prepare_markdown_flattens_before_pandoc() {
        let raw = "### INTENT-SCAN entry\n";
        let out = prepare_markdown_for_pandoc(raw);
        assert_eq!(out.trim(), "**INTENT-SCAN entry**");
    }

    #[test]
    fn skips_markdown_with_existing_extension() {
        let dir =
            std::env::temp_dir().join(format!("middleton-export-skip-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::write(dir.join("A.md"), "# A").unwrap();
        fs::write(dir.join("B.md"), "# B").unwrap();
        fs::write(dir.join("A.epub"), "fake").unwrap();

        let all = collect_markdown_files(&dir, false, "epub").unwrap();
        assert_eq!(all.len(), 2);

        let missing = collect_markdown_files(&dir, true, "epub").unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].file_stem().unwrap().to_str().unwrap(), "B");

        let _ = fs::remove_dir_all(&dir);
    }
}
