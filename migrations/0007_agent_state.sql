-- W00 P00 agent persistence.  Every row carries the explicit
-- operator/tenant/project scope so each repository query can enforce the same
-- boundary as the domain contract.  RLS itself remains deliberately deferred
-- (see 0004); these columns are the current enforcement point, not an
-- optimization to be skipped once policies exist.
--
-- Message.turn_id and Turn.run_id are mutually referencing with the row they
-- point at, so those two foreign keys are DEFERRABLE INITIALLY DEFERRED and
-- checked at commit, following the pattern already used in 0005.  The rest are
-- immediate: the referenced row is always written first inside the same
-- transaction.

CREATE TABLE agent_conversations (
    conversation_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    created_by UUID,
    title TEXT,
    status TEXT NOT NULL CHECK (status IN ('active', 'archived')),
    revision BIGINT NOT NULL CHECK (revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    CHECK (title IS NULL OR length(title) BETWEEN 1 AND 200),
    UNIQUE (operator_id, tenant_id, project_id, conversation_id)
);
CREATE INDEX agent_conversations_scope_updated_idx
    ON agent_conversations (operator_id, tenant_id, project_id, updated_at DESC);

CREATE TABLE agent_messages (
    message_id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    turn_id UUID,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system', 'tool')),
    content TEXT NOT NULL CHECK (length(content) <= 100000),
    metadata JSONB NOT NULL DEFAULT 'null'::JSONB,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, conversation_id)
        REFERENCES agent_conversations (operator_id, tenant_id, project_id, conversation_id)
        ON DELETE CASCADE,
    UNIQUE (operator_id, tenant_id, project_id, conversation_id, sequence),
    UNIQUE (operator_id, tenant_id, project_id, message_id)
);
CREATE INDEX agent_messages_scope_conversation_idx
    ON agent_messages (operator_id, tenant_id, project_id, conversation_id, sequence);

CREATE TABLE agent_turns (
    turn_id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    root_message_id UUID NOT NULL,
    previous_turn_id UUID,
    run_id UUID,
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    cancel_version BIGINT NOT NULL DEFAULT 0 CHECK (cancel_version >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, conversation_id)
        REFERENCES agent_conversations (operator_id, tenant_id, project_id, conversation_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, root_message_id)
        REFERENCES agent_messages (operator_id, tenant_id, project_id, message_id)
        ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, previous_turn_id)
        REFERENCES agent_turns (operator_id, tenant_id, project_id, turn_id)
        ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, turn_id)
);
CREATE INDEX agent_turns_scope_conversation_idx
    ON agent_turns (operator_id, tenant_id, project_id, conversation_id, created_at DESC);
-- A conversation has at most one active turn; the repository also checks this
-- under a row lock so the caller gets a conflict instead of a unique violation.
CREATE UNIQUE INDEX agent_turns_active_per_conversation_idx
    ON agent_turns (operator_id, tenant_id, project_id, conversation_id)
    WHERE status IN ('queued', 'running');

CREATE TABLE agent_runs (
    run_id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL,
    turn_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    capability JSONB NOT NULL,
    error JSONB,
    cancel_version BIGINT NOT NULL DEFAULT 0 CHECK (cancel_version >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, conversation_id)
        REFERENCES agent_conversations (operator_id, tenant_id, project_id, conversation_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, turn_id)
        REFERENCES agent_turns (operator_id, tenant_id, project_id, turn_id)
        ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, run_id)
);
CREATE INDEX agent_runs_scope_conversation_idx
    ON agent_runs (operator_id, tenant_id, project_id, conversation_id, created_at DESC);

-- Attachments are immutable references to the object store.  Only the reference
-- is persisted here; bytes and credentials never enter these tables.
CREATE TABLE agent_message_attachments (
    attachment_id UUID PRIMARY KEY,
    message_id UUID NOT NULL,
    conversation_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    object_id TEXT NOT NULL CHECK (length(object_id) BETWEEN 1 AND 500),
    filename TEXT NOT NULL CHECK (length(filename) BETWEEN 1 AND 512),
    media_type TEXT,
    size_bytes BIGINT CHECK (size_bytes IS NULL OR (size_bytes >= 0 AND size_bytes <= 104857600)),
    sha256 TEXT CHECK (sha256 IS NULL OR sha256 ~ '^[0-9a-f]{64}$'),
    object_version TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, conversation_id)
        REFERENCES agent_conversations (operator_id, tenant_id, project_id, conversation_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, message_id)
        REFERENCES agent_messages (operator_id, tenant_id, project_id, message_id)
        ON DELETE CASCADE,
    UNIQUE (operator_id, tenant_id, project_id, message_id, ordinal)
);

-- `(run_id, checkpoint_scope, step_key)` is unique.  The stored input digest is
-- what makes a checkpoint reusable: a different digest is a conflict, so a
-- resumed branch can never reuse a result produced for other inputs.
CREATE TABLE agent_checkpoints (
    checkpoint_id UUID PRIMARY KEY,
    run_id UUID NOT NULL,
    conversation_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    checkpoint_scope TEXT NOT NULL CHECK (length(checkpoint_scope) BETWEEN 1 AND 200),
    step_key TEXT NOT NULL CHECK (length(step_key) BETWEEN 1 AND 200),
    input_hash TEXT NOT NULL CHECK (length(input_hash) BETWEEN 1 AND 512),
    result_ref JSONB,
    version BIGINT NOT NULL CHECK (version > 0),
    state JSONB NOT NULL DEFAULT 'null'::JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, conversation_id)
        REFERENCES agent_conversations (operator_id, tenant_id, project_id, conversation_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, run_id)
        REFERENCES agent_runs (operator_id, tenant_id, project_id, run_id)
        ON DELETE CASCADE,
    UNIQUE (operator_id, tenant_id, project_id, run_id, checkpoint_scope, step_key)
);

CREATE TABLE agent_tool_call_ledger (
    ledger_entry_id UUID PRIMARY KEY,
    run_id UUID NOT NULL,
    turn_id UUID NOT NULL,
    conversation_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    tool_call_id TEXT NOT NULL CHECK (length(tool_call_id) BETWEEN 1 AND 200),
    tool_name TEXT NOT NULL CHECK (length(tool_name) BETWEEN 1 AND 200),
    arguments_hash TEXT NOT NULL CHECK (length(arguments_hash) BETWEEN 1 AND 512),
    idempotency_key_hash TEXT NOT NULL CHECK (length(idempotency_key_hash) BETWEEN 1 AND 512),
    permission TEXT NOT NULL CHECK (permission IN ('allowed', 'denied')),
    budget TEXT NOT NULL CHECK (budget IN ('allowed', 'denied')),
    intent JSONB NOT NULL DEFAULT 'null'::JSONB,
    attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    result_ref JSONB,
    outcome TEXT NOT NULL CHECK (outcome IN ('intent', 'attempted', 'succeeded', 'failed', 'unknown')),
    cost_minor BIGINT CHECK (cost_minor IS NULL OR cost_minor >= 0),
    currency TEXT CHECK (currency IS NULL OR currency ~ '^[A-Z]{3}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, conversation_id)
        REFERENCES agent_conversations (operator_id, tenant_id, project_id, conversation_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, run_id)
        REFERENCES agent_runs (operator_id, tenant_id, project_id, run_id)
        ON DELETE CASCADE,
    UNIQUE (operator_id, tenant_id, project_id, run_id, tool_call_id)
);
CREATE INDEX agent_tool_call_ledger_scope_run_idx
    ON agent_tool_call_ledger (operator_id, tenant_id, project_id, run_id, created_at);

-- The durable cursor used both for replay and for live SSE delivery.  The
-- per-conversation sequence is allocated inside the transaction that already
-- holds the conversation row lock, and this constraint is the backstop.
CREATE TABLE agent_conversation_events (
    event_id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    event_type TEXT NOT NULL CHECK (length(event_type) BETWEEN 1 AND 100),
    turn_id UUID,
    run_id UUID,
    payload JSONB NOT NULL DEFAULT 'null'::JSONB,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, conversation_id)
        REFERENCES agent_conversations (operator_id, tenant_id, project_id, conversation_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, turn_id)
        REFERENCES agent_turns (operator_id, tenant_id, project_id, turn_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, run_id)
        REFERENCES agent_runs (operator_id, tenant_id, project_id, run_id)
        ON DELETE CASCADE,
    UNIQUE (operator_id, tenant_id, project_id, conversation_id, sequence)
);
CREATE INDEX agent_conversation_events_scope_conversation_idx
    ON agent_conversation_events (operator_id, tenant_id, project_id, conversation_id, sequence);

-- The stored acceptance is what makes a repeated Idempotency-Key return the
-- identical response after a restart; the request hash is what makes the same
-- key with a different body a conflict instead of a replay.
CREATE TABLE agent_submissions (
    submission_id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    idempotency_key_hash TEXT NOT NULL CHECK (length(idempotency_key_hash) BETWEEN 1 AND 512),
    request_hash TEXT NOT NULL CHECK (length(request_hash) BETWEEN 1 AND 512),
    message_id UUID NOT NULL,
    turn_id UUID NOT NULL,
    run_id UUID NOT NULL,
    acceptance JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, conversation_id)
        REFERENCES agent_conversations (operator_id, tenant_id, project_id, conversation_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, message_id)
        REFERENCES agent_messages (operator_id, tenant_id, project_id, message_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, turn_id)
        REFERENCES agent_turns (operator_id, tenant_id, project_id, turn_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, run_id)
        REFERENCES agent_runs (operator_id, tenant_id, project_id, run_id)
        ON DELETE CASCADE,
    UNIQUE (operator_id, tenant_id, project_id, conversation_id, idempotency_key_hash)
);
CREATE INDEX agent_submissions_scope_conversation_idx
    ON agent_submissions (operator_id, tenant_id, project_id, conversation_id, created_at DESC);

-- Deferred because the message and its turn are inserted in the same
-- transaction and reference each other.
ALTER TABLE agent_messages
    ADD CONSTRAINT agent_messages_turn_scope_fk
        FOREIGN KEY (operator_id, tenant_id, project_id, turn_id)
        REFERENCES agent_turns (operator_id, tenant_id, project_id, turn_id)
        ON DELETE RESTRICT
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE agent_turns
    ADD CONSTRAINT agent_turns_run_scope_fk
        FOREIGN KEY (operator_id, tenant_id, project_id, run_id)
        REFERENCES agent_runs (operator_id, tenant_id, project_id, run_id)
        ON DELETE RESTRICT
        DEFERRABLE INITIALLY DEFERRED;
