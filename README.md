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

PDF text imports use an optional isolated parser; see
[`docs/pdf-import.md`](docs/pdf-import.md) for the Compose profile, reusable
Java 21 CI artifact, page-level evidence and current limitations.

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

### Windows V8 CI artifact

The Windows/MSVC CI job uploads the exact prebuilt `rusty_v8` archives needed by
the locked Rust dependency version. This is intended for local development: it
avoids rebuilding V8 or downloading it again during every Cargo invocation.

After a successful Windows CI run, download the artifact with GitHub CLI and
restore it into the local Cargo cache:

```powershell
$commit = git rev-parse HEAD
$repository = "memeloop-online/Memeloop-GEO-Master"
$runs = gh run list --repo $repository --workflow ci.yml --commit $commit --status completed --limit 20 --json databaseId,headSha,status,conclusion
$run = $runs |
  ConvertFrom-Json |
  Select-Object -First 1
if (-not $run) {
  throw "No completed CI run found for commit $commit."
}
if ($run.headSha -ne $commit) {
  throw "The selected CI run does not match commit $commit."
}
$artifact = "rusty-v8-msvc-$($run.headSha)"
$download = Join-Path (Get-Location) ".artifacts\$artifact"
gh run download $run.databaseId --repo $repository --name $artifact --dir $download
if ($LASTEXITCODE -ne 0) { throw "The verified Windows artifact is unavailable." }
pwsh ./scripts/fetch-v8-artifact.ps1 -ArtifactDirectory $download
cargo check --workspace
```

The restore script verifies every archive against `rusty_v8.sha256`, requires
`x86_64-pc-windows-msvc`, and copies only verified `.lib.gz` files to
`$env:CARGO_HOME\.rusty_v8` (or the default Cargo home). Linux bundle artifacts
are deliberately rejected and must never be used as a Windows V8 cache.
The artifact is uploaded only after download and restore verification succeed;
it remains usable when a separate Linux check or subsequent test fails.

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
