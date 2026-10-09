# syntax=docker/dockerfile:1.7

# Generate the approved, self-contained MemeLoop artifacts from the locked
# workspace dependencies. The generated files are copied to the API image
# below; no model endpoint or credential is part of this stage.
FROM node:22.17.0-bookworm-slim AS bundle-builder

WORKDIR /workspace
ENV COREPACK_HOME=/tmp/corepack

RUN corepack enable \
    && corepack prepare pnpm@10.17.1 --activate

COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY apps/web/package.json apps/web/package.json
COPY packages/agent-runtime/package.json packages/agent-runtime/package.json
COPY packages/browser-runner/package.json packages/browser-runner/package.json

RUN pnpm install --frozen-lockfile --filter @memeloop-geo/web

COPY scripts/bundle-agent.mjs scripts/bundle-agent.mjs
COPY packages/agent-runtime/THIRD_PARTY_NOTICES.md packages/agent-runtime/THIRD_PARTY_NOTICES.md
COPY packages/agent-runtime/src packages/agent-runtime/src

RUN pnpm agent:bundle \
    && cd packages/agent-runtime/dist \
    && sha256sum \
      memeloop-agent-loop.bundle.mjs \
      memeloop-content-workflow.bundle.mjs \
      > SHA256SUMS

# cargo-chef (Apache-2.0 OR MIT) owns workspace dependency preparation.
# Pin the tool and share one Rust toolchain/workdir across prepare, cook and build.
FROM rust:1.96-bookworm AS api-chef

WORKDIR /workspace

RUN cargo install cargo-chef --version 0.1.78 --locked

FROM api-chef AS api-planner

COPY Cargo.toml Cargo.lock ./
COPY crates crates

RUN cargo chef prepare --recipe-path recipe.json

# Cache registry dependencies (including V8) separately from application sources.
# Keep artifacts in normal layers so CI's exported BuildKit cache can reuse them.
FROM api-chef AS api-builder

COPY --from=api-planner /workspace/recipe.json recipe.json
RUN cargo chef cook --locked --release -p geo-app --recipe-path recipe.json

# Compile the real API and its embedded migration/prompt files after dependency
# cooking. Runtime model configuration remains deployment configuration.
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY migrations migrations
COPY packages/browser-runner/src/observation-extraction-prompt.txt packages/browser-runner/src/observation-extraction-prompt.txt

RUN cargo build --locked --release -p geo-app

# Keep the runtime small and non-secret. The Rust/V8 binary may need the C++
# runtime in addition to glibc; CA roots are required for configured HTTPS
# model/token services.
FROM debian:bookworm-slim AS api

RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
      ca-certificates \
      libgcc-s1 \
      libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 geo \
    && useradd --uid 10001 --gid 10001 --home-dir /nonexistent --shell /usr/sbin/nologin geo \
    && mkdir -p /opt/geo/bundles \
      /usr/share/doc/memeloop-geo \
    && chown -R geo:geo /opt/geo /usr/share/doc/memeloop-geo

WORKDIR /opt/geo

COPY --chown=10001:10001 --from=api-builder /workspace/target/release/geo-app /usr/local/bin/geo-app
COPY --chown=10001:10001 --from=bundle-builder /workspace/packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs /opt/geo/bundles/memeloop-agent-loop.bundle.mjs
COPY --chown=10001:10001 --from=bundle-builder /workspace/packages/agent-runtime/dist/memeloop-content-workflow.bundle.mjs /opt/geo/bundles/memeloop-content-workflow.bundle.mjs
COPY --chown=10001:10001 --from=bundle-builder /workspace/packages/agent-runtime/dist/SHA256SUMS /opt/geo/bundles/SHA256SUMS
COPY --chown=10001:10001 --from=bundle-builder /workspace/packages/agent-runtime/THIRD_PARTY_NOTICES.md /usr/share/doc/memeloop-geo/agent-runtime-THIRD_PARTY_NOTICES.md
COPY --chown=10001:10001 crates/api/THIRD_PARTY_NOTICES.md /usr/share/doc/memeloop-geo/api-THIRD_PARTY_NOTICES.md
COPY --chown=10001:10001 LICENSE /usr/share/doc/memeloop-geo/LICENSE

USER 10001:10001
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/geo-app"]
