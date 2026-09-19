# Memeloop GEO

Memeloop GEO is an independently implemented platform for the full Generative
Engine Optimization lifecycle: governed enterprise knowledge, baseline
measurement, strategy, content, automated checks, channel publication,
verification, and continuous re-measurement.

The repository is a pnpm/Rust monorepo. The current implementation includes
the shared platform foundation, atomic project start, the first enterprise
knowledge vertical slice, and the P00 AI workbench foundation.

## Project documentation

- [`docs/product-plan-v1.md`](docs/product-plan-v1.md) is the complete product
  and engineering specification.
- [`docs/TODO.md`](docs/TODO.md) contains only unfinished work and current
  acceptance targets.
- [`docs/WORKLOG.md`](docs/WORKLOG.md) records completed work, verification,
  constraints, and technical decisions.
- [`docs/HANDOFF.md`](docs/HANDOFF.md) is the standalone engineering handoff
  for continuing without prior conversation context.

## Local development

Prerequisites: Docker Desktop with Compose, Node.js 22 or newer with Corepack,
pnpm 10, and a Rust toolchain compatible with Rust 1.96 or newer.

```powershell
Copy-Item .env.example .env
docker compose up -d
docker compose ps
pnpm install
```

The local dependency endpoints are PostgreSQL on `localhost:5432`, NATS
JetStream on `localhost:4222` (monitoring on `8222`), Redis on `localhost:6379`,
and MinIO on `localhost:9000` (console on `9001`). The matching application
variables are documented in [`.env.example`](.env.example).

Once the frontend and Rust workspaces are present, use the root commands:

```powershell
$env:GEO_DEV_PASSWORD = "choose-a-local-password"
cargo run -p geo-app
```

In a second terminal, start the browser application. Vite keeps the browser
same-origin and proxies `/api` to the loopback API:

```powershell
pnpm --dir apps/web dev
```

Run the verification suites with:

```powershell
pnpm format:check
pnpm typecheck
pnpm test
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

Stop local dependencies with `docker compose down`. Named volumes retain local
data; remove them only when an intentional clean reset is required:
`docker compose down --volumes`.

## Security and deployment boundary

`compose.yaml` is intentionally for a single developer machine. Its sample
credentials, exposed ports, and storage settings are not suitable for any
shared, staging, or production environment. Production deployments use
dedicated or explicitly isolated PostgreSQL, Redis, NATS, and S3 resources;
secrets are injected through the deployment secret manager and never exposed to
the browser, logs, or messages.

## License

This project is licensed under the [Apache License 2.0](LICENSE). See
[CONTRIBUTING.md](CONTRIBUTING.md) before submitting a contribution.
