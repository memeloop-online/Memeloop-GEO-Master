ALTER TABLE browser_operations ADD COLUMN login_verified_at timestamptz;

CREATE TABLE operator_login_queues (
    queue_id uuid PRIMARY KEY,
    operator_id uuid NOT NULL,
    creator_user_id uuid NOT NULL,
    import_key text NOT NULL,
    status text NOT NULL CHECK (status IN ('running','paused','completed')),
    version bigint NOT NULL CHECK (version > 0),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE UNIQUE INDEX operator_login_queues_active_creator ON operator_login_queues(operator_id,creator_user_id) WHERE status <> 'completed';
CREATE INDEX operator_login_queues_scope ON operator_login_queues(operator_id,creator_user_id,created_at,queue_id);
CREATE TABLE operator_login_queue_items (
    queue_id uuid NOT NULL REFERENCES operator_login_queues(queue_id),
    position integer NOT NULL CHECK (position >= 0),
    row_key text NOT NULL,
    account_id uuid NOT NULL,
    status text NOT NULL CHECK (status IN ('pending','current','succeeded','skipped')),
    attempt integer NOT NULL CHECK (attempt >= 0),
    operation_id uuid REFERENCES browser_operations(operation_id),
    owner_session_id uuid,
    error_code text,
    PRIMARY KEY(queue_id,position),
    UNIQUE(queue_id,row_key),
    UNIQUE(queue_id,account_id),
    CHECK ((operation_id IS NULL) = (owner_session_id IS NULL)),
    CHECK ((status = 'current') = (operation_id IS NOT NULL)),
    CHECK (error_code IS NULL OR error_code IN ('login_not_completed','login_failed','identity_mismatch','network_unavailable','account_disabled','start_failed','runner_unavailable','session_recovery_required','disposal_unconfirmed','browser_unavailable'))
);
CREATE UNIQUE INDEX operator_login_queue_one_current ON operator_login_queue_items(queue_id) WHERE status='current';
