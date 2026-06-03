use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use crate::export_common::{
    collect_markdown_files, ensure_pandoc, existing_dejavu_font_paths, read_and_prepare_markdown,
    title_from_markdown_path,
};

const EPUB_CSS: &str = include_str!("../assets/epub/style.css");

static WARNED_MISSING_FONTS: AtomicBool = AtomicBool::new(false);

pub fn export_markdown_epubs(middleton_dir: &Path, pandoc: &str) -> Result<Vec<PathBuf>> {
    export_markdown_epubs_with_options(middleton_dir, pandoc, false)
}

pub fn export_missing_markdown_epubs(middleton_dir: &Path, pandoc: &str) -> Result<Vec<PathBuf>> {
    export_markdown_epubs_with_options(middleton_dir, pandoc, true)
}

pub fn export_markdown_file(
    middleton_dir: &Path,
    pandoc: &str,
    markdown: &Path,
) -> Result<PathBuf> {
    ensure_pandoc(pandoc)?;
    let css = write_epub_css(middleton_dir)?;
    let epub = markdown.with_extension("epub");
    convert_markdown_to_epub(pandoc, &css, markdown, &epub)?;
    Ok(epub)
}

fn export_markdown_epubs_with_options(
    middleton_dir: &Path,
    pandoc: &str,
    skip_existing: bool,
) -> Result<Vec<PathBuf>> {
    ensure_pandoc(pandoc)?;

    let markdown_files = collect_markdown_files(middleton_dir, skip_existing, "epub")?;
    if markdown_files.is_empty() {
        if skip_existing {
            info!(dir = %middleton_dir.display(), "all markdown files already have epubs");
        } else {
            warn!(dir = %middleton_dir.display(), "no markdown files found to export");
        }
        return Ok(Vec::new());
    }

    let css = write_epub_css(middleton_dir)?;
    let mut epubs = Vec::with_capacity(markdown_files.len());
    for markdown in markdown_files {
        let epub = markdown.with_extension("epub");
        convert_markdown_to_epub(pandoc, &css, &markdown, &epub)?;
        info!(
            markdown = %markdown.display(),
            epub = %epub.display(),
            "exported epub"
        );
        epubs.push(epub);
    }

    Ok(epubs)
}

fn write_epub_css(middleton_dir: &Path) -> Result<PathBuf> {
    let css = middleton_dir.join(".middleton-export-epub.css");
    fs::write(&css, EPUB_CSS).with_context(|| format!("write epub css {}", css.display()))?;
    Ok(css)
}

fn convert_markdown_to_epub(pandoc: &str, css: &Path, markdown: &Path, epub: &Path) -> Result<()> {
    let title = title_from_markdown_path(markdown);
    let prepared_path = read_and_prepare_markdown(markdown)?;

    let mut command = Command::new(pandoc);
    command
        .arg(&prepared_path)
        .arg("-o")
        .arg(epub)
        .arg("--standalone")
        .arg("--from=markdown-raw_tex")
        .arg("--to=epub3")
        .arg("--number-sections")
        .arg("--toc")
        .arg("--toc-depth=2")
        .arg("--highlight-style=tango")
        .arg("--css")
        .arg(css)
        .arg("-M")
        .arg(format!("title={title}"))
        .arg("-M")
        .arg("author=middleton")
        .arg("-M")
        .arg("date=")
        .arg("-M")
        .arg("lang=en");

    let fonts = existing_dejavu_font_paths();
    for font in &fonts {
        command.arg("--epub-embed-font").arg(*font);
    }

    if fonts.is_empty() && !WARNED_MISSING_FONTS.swap(true, Ordering::Relaxed) {
        warn!("DejaVu fonts not found; EPUB export will use reader fallback fonts");
    }

    let output = command
        .output()
        .with_context(|| format!("run pandoc for {}", markdown.display()))?;

    let _ = fs::remove_file(&prepared_path);

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("failed to export {} to EPUB\n{stderr}", markdown.display());
}
