use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use crate::trial;

const PDF_HEADER_XELATEX: &str = include_str!("../assets/pdf/header.tex");
const PDF_HEADER_PDFLATEX: &str = include_str!("../assets/pdf/header-pdflatex.tex");

pub fn export_markdown_pdfs(middleton_dir: &Path, pandoc: &str) -> Result<Vec<PathBuf>> {
    export_markdown_pdfs_with_options(middleton_dir, pandoc, false)
}

pub fn export_missing_markdown_pdfs(middleton_dir: &Path, pandoc: &str) -> Result<Vec<PathBuf>> {
    export_markdown_pdfs_with_options(middleton_dir, pandoc, true)
}

/// Export one markdown file to PDF using the same pandoc settings as phase reports.
pub fn export_markdown_file(
    middleton_dir: &Path,
    pandoc: &str,
    markdown: &Path,
) -> Result<PathBuf> {
    ensure_pandoc(pandoc)?;

    let header_xelatex = middleton_dir.join(".middleton-export-header.tex");
    let header_pdflatex = middleton_dir.join(".middleton-export-header-pdflatex.tex");
    fs::write(&header_xelatex, PDF_HEADER_XELATEX)
        .with_context(|| format!("write pandoc header {}", header_xelatex.display()))?;
    fs::write(&header_pdflatex, PDF_HEADER_PDFLATEX)
        .with_context(|| format!("write pandoc header {}", header_pdflatex.display()))?;

    let pdf = markdown.with_extension("pdf");
    convert_markdown_to_pdf(pandoc, &header_xelatex, &header_pdflatex, markdown, &pdf)?;
    Ok(pdf)
}

fn export_markdown_pdfs_with_options(
    middleton_dir: &Path,
    pandoc: &str,
    skip_existing: bool,
) -> Result<Vec<PathBuf>> {
    ensure_pandoc(pandoc)?;

    let markdown_files = collect_markdown_files(middleton_dir, skip_existing)?;
    if markdown_files.is_empty() {
        if skip_existing {
            info!(dir = %middleton_dir.display(), "all markdown files already have pdfs");
        } else {
            warn!(dir = %middleton_dir.display(), "no markdown files found to export");
        }
        return Ok(Vec::new());
    }

    let header_xelatex = middleton_dir.join(".middleton-export-header.tex");
    let header_pdflatex = middleton_dir.join(".middleton-export-header-pdflatex.tex");
    fs::write(&header_xelatex, PDF_HEADER_XELATEX)
        .with_context(|| format!("write pandoc header {}", header_xelatex.display()))?;
    fs::write(&header_pdflatex, PDF_HEADER_PDFLATEX)
        .with_context(|| format!("write pandoc header {}", header_pdflatex.display()))?;

    let mut pdfs = Vec::with_capacity(markdown_files.len());
    for markdown in markdown_files {
        let pdf = markdown.with_extension("pdf");
        convert_markdown_to_pdf(pandoc, &header_xelatex, &header_pdflatex, &markdown, &pdf)?;
        info!(
            markdown = %markdown.display(),
            pdf = %pdf.display(),
            "exported pdf"
        );
        pdfs.push(pdf);
    }

    Ok(pdfs)
}

fn ensure_pandoc(pandoc: &str) -> Result<()> {
    let output = Command::new(pandoc)
        .arg("--version")
        .output()
        .with_context(|| format!("run `{pandoc} --version`"))?;

    if output.status.success() {
        return Ok(());
    }

    bail!("`{pandoc}` is unavailable or failed. Install pandoc and a PDF engine such as xelatex.");
}

fn collect_markdown_files(middleton_dir: &Path, skip_existing: bool) -> Result<Vec<PathBuf>> {
    let mut files = fs::read_dir(middleton_dir)
        .with_context(|| format!("read directory {}", middleton_dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                && !trial::is_trial_source(path)
        })
        .filter(|path| !skip_existing || !path.with_extension("pdf").exists())
        .collect::<Vec<_>>();

    files.sort();
    Ok(files)
}

/// Replace control characters that break LaTeX/pdf engines with visible escapes.
fn sanitize_markdown_for_latex(input: &str) -> String {
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

fn convert_markdown_to_pdf(
    pandoc: &str,
    header_xelatex: &Path,
    header_pdflatex: &Path,
    markdown: &Path,
    pdf: &Path,
) -> Result<()> {
    let title = title_from_markdown_path(markdown);
    let raw = fs::read_to_string(markdown)
        .with_context(|| format!("read markdown {}", markdown.display()))?;
    let sanitized = sanitize_markdown_for_latex(&raw);
    let sanitized_markdown = markdown.with_extension("md.middleton-export");
    fs::write(&sanitized_markdown, &sanitized)
        .with_context(|| format!("write sanitized markdown {}", sanitized_markdown.display()))?;

    let mut command = Command::new(pandoc);
    command
        .arg(&sanitized_markdown)
        .arg("-o")
        .arg(pdf)
        .arg("--standalone")
        .arg("--from=markdown")
        .arg("--to=pdf")
        .arg("--pdf-engine=xelatex")
        .arg("--number-sections")
        .arg("--toc")
        .arg("--toc-depth=3")
        .arg("--highlight-style=tango")
        .arg("--include-in-header")
        .arg(header_xelatex)
        .arg("-V")
        .arg("geometry:margin=2.4cm")
        .arg("-V")
        .arg("fontsize=11pt")
        .arg("-V")
        .arg("documentclass=article")
        .arg("-M")
        .arg(format!("title={title}"))
        .arg("-M")
        .arg("author=middleton")
        .arg("-M")
        .arg("date=");

    let output = command
        .output()
        .with_context(|| format!("run pandoc for {}", markdown.display()))?;

    if output.status.success() {
        let _ = fs::remove_file(&sanitized_markdown);
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    warn!(
        markdown = %markdown.display(),
        "xelatex export failed; retrying with pdflatex"
    );

    let mut fallback = Command::new(pandoc);
    fallback
        .arg(&sanitized_markdown)
        .arg("-o")
        .arg(pdf)
        .arg("--standalone")
        .arg("--from=markdown")
        .arg("--to=pdf")
        .arg("--pdf-engine=pdflatex")
        .arg("--number-sections")
        .arg("--toc")
        .arg("--toc-depth=3")
        .arg("--highlight-style=tango")
        .arg("--include-in-header")
        .arg(header_pdflatex)
        .arg("-V")
        .arg("geometry:margin=2.4cm")
        .arg("-V")
        .arg("fontsize=11pt")
        .arg("-V")
        .arg("documentclass=article")
        .arg("-M")
        .arg(format!("title={title}"))
        .arg("-M")
        .arg("author=middleton")
        .arg("-M")
        .arg("date=");

    let fallback_output = fallback
        .output()
        .with_context(|| format!("run pandoc fallback for {}", markdown.display()))?;

    if fallback_output.status.success() {
        let _ = fs::remove_file(&sanitized_markdown);
        return Ok(());
    }

    let fallback_stderr = String::from_utf8_lossy(&fallback_output.stderr);
    let _ = fs::remove_file(&sanitized_markdown);
    bail!(
        "failed to export {} to PDF\nxelatex: {stderr}\npdflatex: {fallback_stderr}",
        markdown.display()
    );
}

fn title_from_markdown_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(format_report_title)
        .unwrap_or_else(|| "Middleton Report".to_string())
}

pub(crate) fn format_report_title(stem: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_report_titles() {
        assert_eq!(format_report_title("INTENT-SCAN-1"), "Intent Scan 1");
        assert_eq!(format_report_title("JUDGEMENT"), "Judgement");
    }

    #[test]
    fn sanitizes_control_characters_for_latex() {
        let input = "magic bytes (ELF `\u{7f}ELF`)";
        let sanitized = sanitize_markdown_for_latex(input);
        assert!(!sanitized.contains('\u{7f}'));
        assert!(sanitized.contains(r"\x7f"));
        assert_eq!(sanitized, "magic bytes (ELF `\\x7fELF`)");
    }

    #[test]
    fn preserves_tabs_and_newlines_when_sanitizing() {
        let input = "line one\nline two\tindented";
        assert_eq!(sanitize_markdown_for_latex(input), input);
    }

    #[test]
    fn skips_markdown_with_existing_pdf() {
        let dir =
            std::env::temp_dir().join(format!("middleton-pdf-skip-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::write(dir.join("A.md"), "# A").unwrap();
        fs::write(dir.join("B.md"), "# B").unwrap();
        fs::write(dir.join("A.pdf"), "fake").unwrap();

        let all = collect_markdown_files(&dir, false).unwrap();
        assert_eq!(all.len(), 2);

        let missing = collect_markdown_files(&dir, true).unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].file_stem().unwrap().to_str().unwrap(), "B");

        let _ = fs::remove_dir_all(&dir);
    }
}
