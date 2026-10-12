# Persisted operator login queue

This is an implementation contract for the account-onboarding backlog, not a change to the product plan. The increment supports an explicit, bounded sequence of imported operator-pool accounts, one current login, refresh/restart recovery, pause, retry and skip. It uses the existing noVNC desktop and browser ownership ledger. Imported username/password envelopes are not verified sessions; this increment does not fill forms or expose those envelopes.

## Identity and ordering

- Queue scope is `(operator_id, creator_user_id)`, derived from authenticated operator-resource-manager identity. Another operator or another user cannot read or mutate the queue.
- The current browser remains scoped to the exact authenticated **session** that created it. Never replace `login_operation_scope.owner_id = auth.session.id` with a user or queue ID.
- Creation accepts one `import_key: String`, an explicit ordered array of `{ row_key, account_id }`, maximum 200 entries, and a client-generated `queue_id` for idempotency. Validate each exact `(operator_id, import_key, row_key) → account_id` mapping against durable imports and enabled pool accounts. Reject duplicate row keys/accounts, missing/failed imports and mismatched pairs. Freeze the ordered snapshot; later imports do not append implicitly.
- Import `ordinal` is not a global order: partial submissions can reuse it. Snapshot position is the array index. Existing account display projections supply labels; queue rows contain no credentials or account identity strings.
- At most one noncompleted queue per creator/operator, and at most one current item per queue. Existing account gates still prevent the same account being used by another queue or operation.

## Domain and storage

Add `domain::operator_login_queue` and `OperatorLoginQueueRepository`, implemented by the existing memory/Pg channel-job repositories and included in the `ChannelJobRepository` supertraits. No new browser service or process-local authoritative queue.

```rust
struct OperatorLoginQueueScope {
    operator_id: OperatorId,
    creator_user_id: Uuid,
}
enum OperatorLoginQueueStatus { Running, Paused, Completed }
enum OperatorLoginQueueItemStatus { Pending, Current, Succeeded, Skipped }
struct OperatorLoginQueueSelection { row_key: String, account_id: Uuid }
struct OperatorLoginQueueItem {
    position: u32,
    row_key: String,
    account_id: Uuid,
    status: OperatorLoginQueueItemStatus,
    attempt: u32,
    operation_id: Option<Uuid>,
    owner_session_id: Option<Uuid>,
    error_code: Option<String>,
}
struct OperatorLoginQueue {
    queue_id: Uuid,
    scope: OperatorLoginQueueScope,
    import_key: String,
    status: OperatorLoginQueueStatus,
    version: i64,
    items: Vec<OperatorLoginQueueItem>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
enum OperatorLoginQueueAction {
    Pause,
    Resume,
    ReconcileCurrent,
    RetryCurrent,
    SkipCurrent,
}
```

Serialized enum values are snake_case. Errors are bounded, allow-listed codes, not upstream messages. Public projections omit creator/session-owner IDs, network secrets and browser-operation internals; return `requires_session_recovery: bool` and current `session_id` only when the caller owns its live browser.

Migration `0054_operator_login_queues.sql`:

| Table | Columns / constraints |
| --- | --- |
| `operator_login_queues` | `queue_id uuid PK`, `operator_id uuid`, `creator_user_id uuid`, `import_key text`, `status text`, `version bigint`, `created_at timestamptz`, `updated_at timestamptz`; partial unique `(operator_id, creator_user_id) WHERE status <> 'completed'` |
| `operator_login_queue_items` | `queue_id uuid FK`, `position integer`, `row_key text`, `account_id uuid`, `status text`, `attempt integer`, `operation_id uuid NULL FK browser_operations`, `owner_session_id uuid NULL`, `error_code text NULL`; PK `(queue_id, position)`, unique `(queue_id,row_key)`, unique `(queue_id,account_id)`, partial unique `(queue_id) WHERE status='current'` |
| `browser_operations` | Add nullable `login_verified_at timestamptz`; include the matching optional field in `BrowserOperation` |

Use CHECK constraints for statuses, nonnegative position/attempt and operation/session ownership pairing. Completed queue snapshots remain readable; indexes support scoped active lookup. Retain old browser-operation rows when an item is retried; a new attempt gets a new operation ID.

The browser verification marker is written in the **same transaction** as `save_owned_login_session` saves verified identity and encrypted storage, after its existing account/gate/scope checks. Memory mode holds the same locks until both are set. No endpoint can set this marker directly. Existing rows remain null: account status, presence of credentials, an HTTP success or a later login never retroactively proves a particular operation succeeded.

Repository surface (all return `Result<_, AppError>`):

```rust
async fn create_login_queue(
    &self, scope: &OperatorLoginQueueScope, queue_id: Uuid,
    import_key: &str, selections: &[OperatorLoginQueueSelection],
    channels: &dyn ChannelRepository, at: DateTime<Utc>,
) -> Result<OperatorLoginQueue, AppError>;
async fn get_login_queue(
    &self, scope: &OperatorLoginQueueScope, queue_id: Uuid,
) -> Result<Option<OperatorLoginQueue>, AppError>;
async fn active_login_queue(
    &self, scope: &OperatorLoginQueueScope,
) -> Result<Option<OperatorLoginQueue>, AppError>;
async fn apply_login_queue_action(
    &self, scope: &OperatorLoginQueueScope, queue_id: Uuid,
    expected_version: i64, action: OperatorLoginQueueAction, at: DateTime<Utc>,
) -> Result<OperatorLoginQueue, AppError>;
async fn begin_login_queue_item(
    &self, scope: &OperatorLoginQueueScope, queue_id: Uuid,
    expected_version: i64, owner_session_id: Uuid,
    input: &BeginBrowserOperation, channels: &dyn ChannelRepository,
    at: DateTime<Utc>,
) -> Result<(OperatorLoginQueue, BrowserOperation), AppError>;
async fn pause_login_queue_error(
    &self, scope: &OperatorLoginQueueScope, queue_id: Uuid,
    expected_version: i64, operation_id: Option<Uuid>,
    error_code: &str, at: DateTime<Utc>,
) -> Result<OperatorLoginQueue, AppError>;
```

Creation validates import/account ownership transactionally (memory uses the supplied channel repository). Same queue ID + identical scope/import key/ordered selection is replay; different input conflicts. Action methods lock the queue row and use version CAS. They inspect linked persisted browser operations themselves, not caller-supplied `verified`/`disposed` booleans. Error writes must compare version and exact current operation, including the no-operation case, so a late failure cannot pause a successor. PostgreSQL uses the shared database transaction rather than delegating writes to the supplied channel repository.

`begin_login_queue_item` atomically marks the earliest pending item current **and** creates its existing `Prepared` browser operation/account gate/network pin plus `PoolLoginSession {session_id, account_id, created_at}` reference. Validate `purpose=Login`, operator-pool scope, selected account and exact session owner. Reuse the existing browser-begin transactional helper, including immutable network binding and reservation checks. Memory holds queue/operation locks across the supplied channel repository's login-reference save; save failure must leave queue/gate/operation unchanged. It must not issue runner calls inside the transaction. A claimed item cannot be dropped merely because the initiating HTTP request disappeared. Queue API must not later call an unconditional `save_pool_login`, which could resurrect a reference after skip/disposal.

Concurrent initial dispatch and Prepared recovery use an atomic browser-repository claim:

```rust
async fn claim_browser_start(
    &self, scope: &BrowserOperationScope, id: Uuid, at: DateTime<Utc>,
) -> Result<Option<BrowserOperation>, AppError>;
```

Only the winner of `Prepared → StartRequested` receives `Some(operation)` and may dispatch the runner start. An already `StartRequested` operation returns `None`; Closing/Disposed conflict. Preserve existing scope/expiry checks and leave the direct-login idempotent `mark_browser_start_requested` method unchanged. A crash after claim but before runner dispatch is an unknown `StartRequested` result: reconnect/status-check or dispose the exact operation, never resend its start.

## Transitions and execution

| Situation | Durable result / allowed next action |
| --- | --- |
| Running, no current item | Claim earliest pending item and prepare operation atomically; then invoke existing start path |
| Same-session refresh | Return the same current session and obtain fresh desktop authorization; never create a second browser |
| API restart with `Prepared` current operation | Resume that persisted operation's start; do not recompute its generation, expiry or network |
| Current `StartRequested` | Reconnect/status-check the same session; a missing/unknown runner result pauses, never automatically sends a replacement start |
| Successful verified credential commit | Marker persists even if API crashes before close; recover by closing the exact operation, without repeating identity capture |
| Verified marker **and** exact operation `Disposed` | Reconcile marks item succeeded and clears current ownership; only a running queue claims the next item |
| `Disposed` without marker | Pause with `login_not_completed`; retry/skip remain available, never count success |
| Pause | Persist paused before returning; do not claim successors. Retain current session and any uncertain closing state |
| Retry current | Request exact current disposal first; only after `Disposed` reset item to pending, increment attempt and clear current operation; resume explicitly and claim a fresh operation |
| Skip current | Request exact current disposal first; only after `Disposed` mark skipped; skip never claims verified success |
| Login/network/identity/start failure | Pause this queue, retain current item/reference and show safe error code; other users' queues remain unaffected |
| No pending/current items | Completed |

`Prepared → Disposed` through the existing proven-unstarted release is valid for retry/skip, never for success. A 404, timeout, lease/20-minute expiry, new runner generation or lost login-reference row is not disposal evidence. Unknown close stays linked and retryable.

New authenticated sessions may inspect their user's queue but cannot access the former desktop. Explicit Retry/Skip is a narrow queue-authorized recovery operation: verify creator/operator and exact linked old operation, request its existing trusted disposal path, and wait for confirmation. Only then may a new current operation use the new authenticated session. Never mutate the old operation's owner. Generic direct login endpoints keep their current access rules.

Every mutating route reconciles before deciding whether it can advance. Resume, successful Complete and Skip can drive one subsequent start in the same request. A bounded `POST /drive` recovers/advances after refresh; GET never starts browsers. UI polls reads and calls drive only for a running queue with no active desktop or pending close reconciliation. Persistent state, not component state, determines the next account. No unattended login, autofill or browser fan-out is implied.

## HTTP and UI contract

All routes require existing operator-resource-manager authorization and normal CSRF/Origin checks:

- `POST /api/v1/operator/channel-login-queues`: `{ queue_id, import_key, items: [{row_key, account_id}] }`; create/replay and drive first item.
- `GET /api/v1/operator/channel-login-queues/active`: queue or null.
- `GET /api/v1/operator/channel-login-queues/{id}`: authoritative projection.
- `POST .../{id}/{drive|pause|resume|retry|skip}`: `{ expected_version }`; return refreshed projection. Retry/skip must retain current on unconfirmed disposal.
- `POST .../{id}/complete`: `{ expected_version }`; verify/save through the same pool-login completion service, close, reconcile and drive next only when still running.

Frozen public DTOs (camelCase TypeScript type names below describe snake_case JSON keys):

```ts
type LoginQueueStatus = "running" | "paused" | "completed";
type LoginQueueItemStatus = "pending" | "current" | "succeeded" | "skipped";
interface LoginQueueItemView {
  position: number;
  row_key: string;
  account_id: string;
  status: LoginQueueItemStatus;
  attempt: number;
  error_code: string | null;
}
interface LoginQueueCurrentView {
  position: number;
  account_id: string;
  session_id: string | null; // only exposed to the live session owner
  phase: "preparing" | "awaiting_login" | "closing" | "recovery_required";
  requires_session_recovery: boolean;
}
interface LoginQueueView {
  queue_id: string;
  import_key: string;
  status: LoginQueueStatus;
  version: number;
  items: LoginQueueItemView[];
  current: LoginQueueCurrentView | null;
  can_drive: boolean;
  created_at: string;
  updated_at: string;
}
```

Successful create/read/action responses are the bare `LoginQueueView`; active lookup returns `LoginQueueView | null`. `can_drive` is computed server-side: running and either no current item, a same-owner Prepared current operation, or a current operation requiring post-verification/terminal reconciliation. It is false for an ordinary awaiting-login desktop, a paused queue or required session recovery. Server revalidates every drive; this boolean is not authorization.

Return 409 for stale version or incompatible transition with no new browser effects. Disposal dependency failure may return 503; the persisted queue remains paused/current and GET exposes recovery state. Complete with the current version on the exact current operation's persisted verification marker skips recapture and retries that operation's disposal. Complete replay after advance has a stale `expected_version`: return 409 without verifying, closing or otherwise acting on the next account. The client rereads the queue projection; no historical version-to-item mapping or successful-replay inference from previously succeeded items is provided.

Queue UI has one reusable `RemoteLogin` for its persisted current session, progress, Pause/Resume and scoped Retry/Skip. Refresh loads active queue. Queue callbacks must use queue routes; a component-local completion/cancel event or optimistic 404 handling must not advance it. Preserve existing direct single-account login. Copy should explain “Log in to this account”, “Waiting for the previous window to close”, or “Session recovery required”; do not promise automatic username/password entry.

## Minimal extraction and ownership

1. Domain/persistence worker: new queue domain/persistence modules, migration, exports/supertrait registration, queue memory storage, verification marker in existing browser repositories, shared atomic browser-begin transaction helper and repository tests.
2. API worker: new `operator_login_queue.rs`, routing/tests; extract pool start/complete logic from `channels.rs` into reusable crate-private service functions without duplicating authorization. Split browser preparation into input construction and persisted begin so queue begin can share generation/network selection and commit linkage atomically. Existing direct wrappers keep their behavior.
3. UI worker: operator import page, queue API/types/components/tests and shared translations. Queue-specific callbacks must not use direct optimistic cancellation. No server/domain files.

Coordinator owns shared documentation, assembly conflict resolution, combined verification and review. The product plan stays unchanged.

Acceptance includes ordered partial-import snapshot, same-key replay/conflict, concurrent drive one-current guarantee, simultaneous drive/Prepared recovery issuing exactly one runner start, cross-user/operator denial, expired-cookie handoff denial until disposal, pause/complete race, crash after credential commit, crash after claim, runner restart/unknown close, disabled account or proxy failure, retry/skip after proven disposal, refresh restoring the same noVNC session, and sequential next-account launch. Run equivalent memory and actual PostgreSQL tests. Synthetic tests do not establish real provider login, autofill or distinct public egress.
