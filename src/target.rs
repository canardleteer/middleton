use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use git2::Repository;
use git2::build::RepoBuilder;
use url::Url;

pub fn resolve_target(input: &str, output: &Option<PathBuf>) -> Result<PathBuf> {
    let input = input.trim();
    let path = Path::new(input);

    if path.is_dir() {
        return path
            .canonicalize()
            .with_context(|| format!("canonicalize directory {}", path.display()));
    }

    if looks_like_git_url(input) {
        let dest = clone_destination(output, input)?;
        return clone_or_reuse(input, &dest);
    }

    bail!("input is neither an existing directory nor a git URL: {input}");
}

fn clone_or_reuse(url: &str, dest: &Path) -> Result<PathBuf> {
    if dest.is_file() {
        bail!(
            "clone destination is a file, not a directory: {}",
            dest.display()
        );
    }

    if dest.is_dir() {
        if Repository::open(dest).is_ok() {
            return dest
                .canonicalize()
                .with_context(|| format!("canonicalize existing repository {}", dest.display()));
        }

        if directory_is_empty(dest)? {
            RepoBuilder::new()
                .clone(url, dest)
                .with_context(|| format!("git clone into empty directory {}", dest.display()))?;
            return dest
                .canonicalize()
                .with_context(|| format!("canonicalize cloned directory {}", dest.display()));
        }

        bail!(
            "clone destination already exists and is not an empty git repository: {} \
             (remove it, choose a different --output, or pass the existing directory as input)",
            dest.display()
        );
    }

    RepoBuilder::new()
        .clone(url, dest)
        .with_context(|| format!("git clone into {}", dest.display()))?;

    dest.canonicalize()
        .with_context(|| format!("canonicalize cloned directory {}", dest.display()))
}

fn directory_is_empty(path: &Path) -> Result<bool> {
    let mut entries =
        fs::read_dir(path).with_context(|| format!("read clone destination {}", path.display()))?;
    Ok(entries.next().is_none())
}

fn looks_like_git_url(input: &str) -> bool {
    input.starts_with("git@")
        || input.starts_with("ssh://")
        || input.starts_with("git://")
        || Url::parse(input)
            .ok()
            .is_some_and(|url| matches!(url.scheme(), "http" | "https"))
}

fn clone_destination(output: &Option<PathBuf>, url: &str) -> Result<PathBuf> {
    if let Some(output) = output {
        return Ok(output.clone());
    }

    let repo_name = repo_name_from_url(url)?;
    Ok(PathBuf::from(format!("./{repo_name}")))
}

fn repo_name_from_url(url: &str) -> Result<String> {
    if url.starts_with("git@") {
        let path = url.split(':').nth(1).context("parse scp-style git URL")?;
        return basename_without_git(path);
    }

    let parsed = Url::parse(url).context("parse git URL")?;
    let path = parsed.path().trim_end_matches('/');
    let name = path
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .context("derive repository name from URL")?;
    basename_without_git(name)
}

fn basename_without_git(name: &str) -> Result<String> {
    let base = Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    Ok(base.trim_end_matches(".git").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_name_from_https_url() {
        assert_eq!(
            repo_name_from_url("https://github.com/org/repo.git").unwrap(),
            "repo"
        );
    }

    #[test]
    fn repo_name_from_scp_url() {
        assert_eq!(
            repo_name_from_url("git@github.com:org/repo.git").unwrap(),
            "repo"
        );
    }

    #[test]
    fn directory_is_empty_detects_entries() {
        let dir = std::env::temp_dir().join(format!("middleton-empty-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(directory_is_empty(&dir).unwrap());
        fs::write(dir.join("marker"), "x").unwrap();
        assert!(!directory_is_empty(&dir).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }
}
