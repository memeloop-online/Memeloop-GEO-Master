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

# Compile only the API binary. Runtime model configuration remains deployment
# configuration, while the generated bundles are supplied from the prior stage.
FROM rust:1.96-bookworm AS api-builder

WORKDIR /workspace

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
    && chown -R geo:geo /opt/geo

WORKDIR /opt/geo

COPY --from=api-builder /workspace/target/release/geo-app /usr/local/bin/geo-app
COPY --from=bundle-builder /workspace/packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs /opt/geo/bundles/memeloop-agent-loop.bundle.mjs
COPY --from=bundle-builder /workspace/packages/agent-runtime/dist/memeloop-content-workflow.bundle.mjs /opt/geo/bundles/memeloop-content-workflow.bundle.mjs
COPY --from=bundle-builder /workspace/packages/agent-runtime/dist/SHA256SUMS /opt/geo/bundles/SHA256SUMS
COPY --from=bundle-builder /workspace/packages/agent-runtime/THIRD_PARTY_NOTICES.md /usr/share/doc/memeloop-geo/agent-runtime-THIRD_PARTY_NOTICES.md
COPY crates/api/THIRD_PARTY_NOTICES.md /usr/share/doc/memeloop-geo/api-THIRD_PARTY_NOTICES.md
COPY LICENSE /usr/share/doc/memeloop-geo/LICENSE

RUN chown -R 10001:10001 /usr/local/bin/geo-app /opt/geo/bundles /usr/share/doc/memeloop-geo

USER 10001:10001
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/geo-app"]
