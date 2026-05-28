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
