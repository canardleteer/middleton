use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AgentKind {
    #[value(name = "opencode")]
    OpenCode,
    #[value(name = "claudecode")]
    ClaudeCode,
    #[value(name = "codex")]
    Codex,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenCode => "opencode",
            Self::ClaudeCode => "claudecode",
            Self::Codex => "codex",
        }
    }
}

/// Corpus lens for the five-phase trial pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ReviewProfile {
    /// Code repositories: docs + implementation, CI, proofs, etc.
    #[default]
    #[value(name = "repository")]
    Repository,
    /// Specification/design packs with little or no source code.
    #[value(name = "documents")]
    Documents,
}

impl ReviewProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::Documents => "documents",
        }
    }

    /// Plan step: guarded web search / fetch (both profiles).
    pub fn plan_allows_web_research(self, permission: &str) -> bool {
        permission.contains("network")
            || permission.contains("fetch")
            || permission.contains("download")
            || permission.contains("search")
    }

    /// Plan step: shell / git via bash (repository profile only).
    pub fn plan_allows_shell(self, permission: &str) -> bool {
        self == Self::Repository
            && (permission.contains("bash")
                || permission.contains("command")
                || permission.contains("terminal"))
    }

    pub fn plan_allows_investigation(self, permission: &str) -> bool {
        self.plan_allows_web_research(permission) || self.plan_allows_shell(permission)
    }

    pub fn plan_allows_command_execution(self) -> bool {
        self == Self::Repository
    }
}

fn is_install_permission(permission: &str) -> bool {
    permission.contains("install")
}

fn is_build_execution_permission(permission: &str) -> bool {
    permission.contains("execute")
        || permission.contains("bash")
        || permission.contains("command")
        || permission.contains("terminal")
        || permission.contains("network")
        || permission.contains("fetch")
        || permission.contains("download")
        || permission.contains("search")
}

/// OpenCode permission reply for a plan or build step.
pub fn opencode_permission_reply(
    profile: ReviewProfile,
    permission: &str,
    step_is_plan: bool,
    writes_middleton_only: bool,
    is_read: bool,
    is_write: bool,
) -> bool {
    if is_install_permission(permission) {
        return false;
    }
    if step_is_plan {
        return is_read || profile.plan_allows_investigation(permission);
    }
    if is_build_execution_permission(permission) {
        return false;
    }
    (is_write && writes_middleton_only) || is_read
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn repository_plan_allows_git_and_web() {
        assert!(ReviewProfile::Repository.plan_allows_shell("bash.execute"));
        assert!(ReviewProfile::Repository.plan_allows_web_research("network.fetch"));
    }

    #[test]
    fn documents_plan_allows_web_only() {
        assert!(!ReviewProfile::Documents.plan_allows_shell("bash.execute"));
        assert!(ReviewProfile::Documents.plan_allows_web_research("network.fetch"));
    }
}
