# Working on Middleton

Middleton is a Rust CLI that reviews repositories or document packs through
OpenCode, Claude Code, or Codex. Intent and depth analysis are followed by
prosecution and defense, then judgement. Each phase has a plan step and a build
step. Reports and audit logs live with session IDs under the reviewed target's
`.middleton/` directory.

## Repository map

- `src/main.rs`: clap arguments and runtime lifecycle; pipeline and exports.
- `src/agent.rs`: backend names, review profiles, and shared permission policy.
- `src/session.rs` and `src/opencode.rs`: OpenCode sessions and server setup.
- `src/claude_agent.rs` and `src/codex_agent.rs`: backend protocol handling,
  approvals, model selection, and report retries.
- `src/prompts.rs`: phase prompts and repository/documents profile differences.
- `src/paths.rs`, `src/output.rs`, `src/manifest.rs`, and `src/run_log.rs`:
  artifact paths, validation, session persistence, and audit logging.
- `src/trial.rs`, `src/export_common.rs`, `src/pdf.rs`, and `src/epub.rs`:
  consolidated reports and pandoc exports; styles are embedded from `assets/`.
- `.github/workflows/`: Rust checks on Linux/macOS, Docker compiler
  compatibility, Markdown linting, and dependency auditing.

## Build and verification

Use the stable toolchain from `rust-toolchain.toml`. Default builds vendor
libgit2 and OpenSSL. You need a C toolchain plus make and Perl. Builds with
`--no-default-features` require the system development libraries instead.
Keep `Cargo.lock` committed and use `--locked` for reproducible checks.

Before committing, run and require success from:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

Run `cargo fmt --all` to apply formatting. For Markdown changes, also run:

```sh
rumdl check README.md AGENTS.md
```

CI pins rumdl to `0.2.62`. When changing dependencies, also run
`cargo audit --deny warnings` with cargo-audit `0.22.1`. The dependency audit
workflow refreshes advisories and runs on dependency changes and weekly.
The Rust workflow checks the compiler version read from `Dockerfile` as well
as stable Rust on Linux and macOS.

`cargo run --locked -- --help` checks the CLI without starting an agent.
Unit tests run locally without API credentials, model calls, pandoc, or TeX.
Live reviews require an installed, authenticated backend and consume its
resources. Use live reviews only when the task calls for them. PDF and EPUB
exports need pandoc; PDFs also need a TeX engine. Docker runtimes are
experimental and are not covered by CI.

## Rust CLI conventions

- Use `clap` derives and typed arguments. Retain a root parser test using
  `clap::CommandFactory::command().debug_assert()`.
- Keep argument parsing, logging setup, and exit handling at the binary
  boundary. Put reusable behavior in a focused public library API when
  extracting it; avoid unrelated reorganizations.
- Document the public API with useful rustdoc that explains behavior and
  errors. Add examples where they help callers.
- Prefer idiomatic Rust expressions and existing project patterns. Use
  `anyhow::Result`. Attach context to process and filesystem errors as well as
  protocol failures.
- Use Tokio for asynchronous work and `tracing` for diagnostics. Initialize
  `tracing-subscriber` at the binary boundary and honor `RUST_LOG`.
- Add tests for changed behavior and important failure paths. Prefer local
  protocol fixtures to paid backend calls.
- When MCP exposure is requested, default to a compatible `clap-mcp` release
  at least `0.1.0-rc.1`, sharing implementations with the CLI. Keep MCP work
  within the requested scope.
- Update the README's options and examples when CLI behavior changes.

## Review rules and invariants

- Preserve the plan/build separation: plans investigate; builds write reports
  from the completed plan. Preserve differences between review profiles.
- Treat the target corpus as read-only. Report writes must stay inside the
  current run directory. Test approval changes for both allowed and denied
  actions; keep confirmed actions in the audit log.
- The Codex client is shared behind a Tokio mutex. Keep each request/event
  sequence together and pass the locked client into retry helpers rather than
  locking the same mutex again.
- Propagate interrupted or failed agent turns. Shut down runtimes on pipeline
  failure and preserve the primary error if shutdown also fails.
- Keep private reviewer notes under `context/`; never merge them into public
  trial reports or exports. Do not log credentials.
- Preserve user-owned `.env*`, `.middleton.toml`, and `.middleton/` content.
  Generated reports are not source files. Do not inspect credentials or
  private reports unless needed for the requested task.

## Agent documentation standards

Maintain this file according to the [AGENTS.md standard](https://agents.md/),
and keep it portable across compatible agent clients, without assumptions
about user-specific paths or session state.
