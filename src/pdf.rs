use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use crate::export_common::{
    collect_markdown_files, ensure_pandoc, read_and_prepare_markdown, title_from_markdown_path,
};

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

    let markdown_files = collect_markdown_files(middleton_dir, skip_existing, "pdf")?;
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

fn convert_markdown_to_pdf(
    pandoc: &str,
    header_xelatex: &Path,
    header_pdflatex: &Path,
    markdown: &Path,
    pdf: &Path,
) -> Result<()> {
    let title = title_from_markdown_path(markdown);
    let prepared_path = read_and_prepare_markdown(markdown)?;

    let mut command = Command::new(pandoc);
    command
        .arg(&prepared_path)
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
        .arg("-V")
        .arg("mainfont=DejaVu Sans")
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
        let _ = fs::remove_file(&prepared_path);
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    warn!(
        markdown = %markdown.display(),
        "xelatex export failed; retrying with pdflatex"
    );

    let mut fallback = Command::new(pandoc);
    fallback
        .arg(&prepared_path)
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

    let _ = fs::remove_file(&prepared_path);

    if fallback_output.status.success() {
        return Ok(());
    }

    let fallback_stderr = String::from_utf8_lossy(&fallback_output.stderr);
    bail!(
        "failed to export {} to PDF\nxelatex: {stderr}\npdflatex: {fallback_stderr}",
        markdown.display()
    );
}
