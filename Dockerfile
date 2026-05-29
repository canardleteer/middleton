# syntax=docker/dockerfile:1

ARG RUST_VERSION=1.96.0
ARG DEBIAN_VERSION=trixie

FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        perl \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .
RUN cargo build --release

ARG DEBIAN_VERSION=trixie
FROM debian:${DEBIAN_VERSION}-slim AS runtime-base

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        fonts-dejavu \
        fonts-lmodern \
        pandoc \
        texlive-fonts-recommended \
        texlive-latex-extra \
        texlive-latex-recommended \
        texlive-xetex \
        unzip \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL https://mirrors.ctan.org/fonts/lm.zip -o /tmp/lm.zip \
    && unzip -q /tmp/lm.zip -d /tmp/lm-extract \
    && cp -a /tmp/lm-extract/lm/fonts /usr/local/share/texmf/ \
    && cp -a /tmp/lm-extract/lm/tex /usr/local/share/texmf/ \
    && mktexlsr \
    && rm -rf /tmp/lm.zip /tmp/lm-extract

COPY --from=builder /build/target/release/middleton /usr/local/bin/middleton

RUN useradd --create-home --shell /bin/bash --uid 1000 middleton

WORKDIR /workspace
ENV PATH="/home/middleton/.local/bin:${PATH}"
USER middleton

# Codex CLI — releases: https://github.com/openai/codex/releases
# Default CODEX_VERSION=0.135.0 → tag rust-v0.135.0
# Linux assets: codex-package-{x86_64,aarch64}-unknown-linux-musl.tar.gz
FROM runtime-base AS codex-runtime

ARG CODEX_VERSION=0.135.0
ARG TARGETARCH

USER middleton
RUN set -eux; \
    case "${TARGETARCH}" in \
        amd64) CODEX_ARCH="x86_64" ;; \
        arm64) CODEX_ARCH="aarch64" ;; \
        *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    CODEX_DIR="/home/middleton/.local/share/codex/${CODEX_VERSION}"; \
    mkdir -p "${CODEX_DIR}"; \
    curl -fsSL \
        "https://github.com/openai/codex/releases/download/rust-v${CODEX_VERSION}/codex-package-${CODEX_ARCH}-unknown-linux-musl.tar.gz" \
        | tar -xzf - -C "${CODEX_DIR}"; \
    mkdir -p /home/middleton/.local/bin; \
    ln -sf "${CODEX_DIR}/bin/codex" /home/middleton/.local/bin/codex; \
    codex --version

ENV PATH="/home/middleton/.local/share/codex/${CODEX_VERSION}/codex-path:${PATH}"

ENTRYPOINT ["middleton"]
CMD ["--help"]

# Claude Code — install & version pinning: https://code.claude.com/docs/en/setup
# Default CLAUDE_VERSION=stable; override with a semver (e.g. 2.1.89)
FROM runtime-base AS claude-runtime

ARG CLAUDE_VERSION=stable

USER middleton
RUN curl -fsSL https://claude.ai/install.sh | bash -s "${CLAUDE_VERSION}" \
    && claude --version

RUN mkdir -p /home/middleton/.claude \
    && printf '%s\n' '{"env":{"DISABLE_AUTOUPDATER":"1"}}' > /home/middleton/.claude/settings.json

ENTRYPOINT ["middleton"]
CMD ["--help"]

# OpenCode — releases: https://github.com/anomalyco/opencode/releases
# Default OPENCODE_VERSION=v1.15.12
FROM runtime-base AS opencode-runtime

ARG OPENCODE_VERSION=v1.15.12
ARG TARGETARCH

USER middleton
RUN set -eux; \
    case "${TARGETARCH}" in \
        amd64) OPENCODE_ASSET="opencode-linux-x64.tar.gz" ;; \
        arm64) OPENCODE_ASSET="opencode-linux-arm64.tar.gz" ;; \
        *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    mkdir -p /home/middleton/.local/bin; \
    curl -fsSL \
        "https://github.com/anomalyco/opencode/releases/download/${OPENCODE_VERSION}/${OPENCODE_ASSET}" \
        | tar -xzf - -C /home/middleton/.local/bin opencode; \
    opencode --version

ENTRYPOINT ["middleton"]
CMD ["--help"]
