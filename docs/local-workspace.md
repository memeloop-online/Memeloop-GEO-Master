# Private persistent local workspace

This optional development helper bootstraps a local-only PostgreSQL-backed workspace and supervises the existing Rust API, browser runner, and Vite. It does not configure external model routes, accounts, platform capabilities, or TLS, and does not establish that an external login or publication succeeds.

Build `geo-app`, install the monorepo packages and pinned Playwright Chromium beforehand. Start Docker Desktop yourself and verify its engine is available. Set `GEO_LOCAL_APP_BINARY` to the absolute built executable and `GEO_LOCAL_STATE_DIR` to a **new absolute directory outside the repository** on a private local disk. Optionally set `GEO_LOCAL_TMP_DIR` to an external scratch directory and `PLAYWRIGHT_BROWSERS_PATH` to an existing pinned browser cache. Do not place the state directory in a synchronized/shared folder. Then run:

```text
node scripts/start-local-workspace.mjs --interactive
```

`--interactive` opens a supervised, headed Playwright Chromium and logs in only to the local first-party application. Connect external accounts yourself through the application's account connection interface; never provide those credentials to this helper. Without `--interactive`, it verifies first-party login with headless Chromium, closes that browser, prints the local URL, and remains supervising. `--check` also verifies the browser's Secure cookie and authenticated session, then closes its own API, runner, Vite, and browser. An installed pinned Chromium cache is required in every mode; the helper never downloads a browser. Ctrl+C closes owned processes; PostgreSQL data and its dedicated container/volume remain in place for subsequent runs. Do not concurrently run two instances with the same state directory.

The default mode requires ports 8080, 5173, 38080, and 15432; external-runner mode does not reserve 38080. It refuses preoccupied service ports and refuses unknown Docker resources. The only Docker resources it creates are its own labeled `postgres:17-alpine` container and volume bound to loopback port 15432. It does not stop, delete, recreate, or replace a container or volume. The bootstrap identity, password, runner token, PostgreSQL password, and session cipher key are generated once in the private state directory and reused across restarts; **back up this directory securely** because losing the cipher key makes stored sessions unreadable. The helper does not display credentials, database URLs, browser storage, screenshots, traces, or remote login activity.

The API retains Secure cookies in PostgreSQL mode. Chromium may accept them on `localhost`; if login fails because of browser cookie handling, use a correctly configured HTTPS ingress. Do not disable Secure cookies to make HTTP work. This helper does not add authentication bypasses or a product login endpoint.

Run the helper's non-Docker unit tests with `node --test scripts/local-workspace.test.mjs`. Its readiness checks do not validate a real external account or official search.

## Use an existing Linux browser runner

Set both server-side variables `GEO_BROWSER_RUNNER_URL` and `GEO_BROWSER_RUNNER_TOKEN` before launching to use an independently managed runner. The URL must be a credential-free HTTP(S) origin without a path prefix, query or fragment, matching the Rust bridge contract. Load the token from private configuration or a secret manager; never put it in a tracked file, frontend variable or command history.

In this mode the helper checks the authenticated capabilities endpoint before starting the API. It neither reserves port 38080 nor starts, supervises or stops the external runner. Missing settings, invalid URLs and a failed capability probe stop startup without falling back to a local runner. The default mode is unchanged when both variables are absent. The generated local identity and session cipher key remain unchanged in either mode.

A loopback port-forward to a private cluster Service is suitable for isolated development. Manage its lifetime separately; stopping the helper does not stop the forward or cluster runner. Configure the runner and token before restarting the API, which reads them at startup. Preserve the existing private state directory and database when switching; active login windows may end, and two executors must not be started against the same live workspace. Capability readiness does not prove real login, official search or publication.

Linux CI additionally runs `node scripts/verify-local-workspace.mjs` with a new, nonexistent external state path and the built application. It launches `--check` twice, checks that owned service ports are released after each run, and compares protected configuration and aggregate PostgreSQL session facts across restarts. Private state is never uploaded; only fixed verification labels are logged. The dedicated container and volume remain until the disposable runner is reclaimed. This test verifies first-party persistence, not external account login or official search.
